use alloc::vec::Vec;

use embedded_graphics::{
    mono_font::{ascii::FONT_9X15, MonoTextStyle},
    prelude::*,
    primitives::{PrimitiveStyleBuilder, Rectangle, RoundedRectangle},
    text::{Alignment, Text},
};
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use t5s3_epaper_core::Display;

use crate::layout::{screen_to_native_rect, SCREEN_W};

// on-screen touch keyboard shared by the lora composer and the wifi password
// entry. the keys are static, so the whole board is painted once on entry (full
// refresh); callers repaint just their input field on each keystroke (partial
// refresh). the enter key's label is caller-supplied ("SEND", "SAVE", ...).
const KB_KEY_W: i32 = 50;
const KB_KEY_H: i32 = 78;
const KB_GAP: i32 = 4;
const KB_GAP_Y: i32 = 8;
// keyboard sits at the bottom of the screen (rows end ~24px from the edge).
const KB_TOP: i32 = 600;
const KB_X: i32 = 2;
const KB_FULL_W: i32 = 536;
const KB_TOGGLE_W: i32 = 90;
const KB_ENTER_W: i32 = 110;

const KB_LETTERS: [&str; 3] = ["qwertyuiop", "asdfghjkl", "zxcvbnm"];
const KB_SYMBOLS: [&str; 3] = ["1234567890", "@#$&-+()/", "*\"':;!?,"];

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Key {
    Char(char),
    Shift,
    Symbols,
    Backspace,
    Space,
    Enter,
}

struct KeyBox {
    key: Key,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

fn kb_row_y(row: i32) -> i32 {
    KB_TOP + row * (KB_KEY_H + KB_GAP_Y)
}

// build the key boxes for the current layer. the letters layer puts a shift key
// at the left of the third row; the symbols layer fills that slot with a symbol
// instead, so both layers share the same nine-slot geometry.
fn keyboard(symbols: bool, shift: bool) -> Vec<KeyBox> {
    let rows = if symbols { KB_SYMBOLS } else { KB_LETTERS };
    let mut keys = Vec::new();

    for (row, &row_keys) in rows.iter().enumerate().take(2) {
        let n = row_keys.chars().count() as i32;
        let ox = (SCREEN_W - (n * KB_KEY_W + (n - 1) * KB_GAP)) / 2;
        let y = kb_row_y(row as i32);
        for (i, c) in row_keys.chars().enumerate() {
            let ch = if !symbols && shift {
                c.to_ascii_uppercase()
            } else {
                c
            };
            keys.push(KeyBox {
                key: Key::Char(ch),
                x: ox + i as i32 * (KB_KEY_W + KB_GAP),
                y,
                w: KB_KEY_W,
                h: KB_KEY_H,
            });
        }
    }

    // third row: nine slots. letters -> [shift][7 letters][del]; symbols ->
    // [8 symbols][del].
    let y = kb_row_y(2);
    let ox = (SCREEN_W - (9 * KB_KEY_W + 8 * KB_GAP)) / 2;
    let mut x = ox;
    if !symbols {
        keys.push(KeyBox {
            key: Key::Shift,
            x,
            y,
            w: KB_KEY_W,
            h: KB_KEY_H,
        });
        x += KB_KEY_W + KB_GAP;
    }
    for c in rows[2].chars() {
        let ch = if !symbols && shift {
            c.to_ascii_uppercase()
        } else {
            c
        };
        keys.push(KeyBox {
            key: Key::Char(ch),
            x,
            y,
            w: KB_KEY_W,
            h: KB_KEY_H,
        });
        x += KB_KEY_W + KB_GAP;
    }
    keys.push(KeyBox {
        key: Key::Backspace,
        x,
        y,
        w: KB_KEY_W,
        h: KB_KEY_H,
    });

    // bottom row: layer toggle, wide space bar, enter.
    let y = kb_row_y(3);
    keys.push(KeyBox {
        key: Key::Symbols,
        x: KB_X,
        y,
        w: KB_TOGGLE_W,
        h: KB_KEY_H,
    });
    let enter_x = KB_X + KB_FULL_W - KB_ENTER_W;
    let space_x = KB_X + KB_TOGGLE_W + KB_GAP;
    keys.push(KeyBox {
        key: Key::Space,
        x: space_x,
        y,
        w: enter_x - KB_GAP - space_x,
        h: KB_KEY_H,
    });
    keys.push(KeyBox {
        key: Key::Enter,
        x: enter_x,
        y,
        w: KB_ENTER_W,
        h: KB_KEY_H,
    });

    keys
}

// hit() polls this once per touch press while a keyboard screen is active, so
// avoid rebuilding all ~30 KeyBox entries (with heap allocation) on every call
// when the (symbols, shift) layer hasn't changed since the last hit(). hit()
// is only ever called from core 0's single UI task, so the unsynchronized
// static is sound.
static mut KB_CACHE: Option<(bool, bool, Vec<KeyBox>)> = None;

fn cached_keyboard(symbols: bool, shift: bool) -> &'static [KeyBox] {
    unsafe {
        let cache = &mut *core::ptr::addr_of_mut!(KB_CACHE);
        if !matches!(cache, Some((s, sh, _)) if *s == symbols && *sh == shift) {
            *cache = Some((symbols, shift, keyboard(symbols, shift)));
        }
        match cache {
            Some((_, _, keys)) => keys,
            None => &[],
        }
    }
}

pub(crate) fn hit(sx: i32, sy: i32, symbols: bool, shift: bool) -> Option<Key> {
    cached_keyboard(symbols, shift)
        .iter()
        .find_map(|k| (sx >= k.x && sx < k.x + k.w && sy >= k.y && sy < k.y + k.h).then_some(k.key))
}

fn key_label<'a>(key: Key, symbols: bool, enter_label: &'a str, buf: &'a mut [u8; 4]) -> &'a str {
    match key {
        Key::Char(c) => c.encode_utf8(buf),
        Key::Shift => "shift",
        Key::Symbols => {
            if symbols {
                "abc"
            } else {
                "123"
            }
        }
        Key::Backspace => "del",
        Key::Space => "space",
        Key::Enter => enter_label,
    }
}

pub(crate) fn draw(display: &mut Display, symbols: bool, shift: bool, enter_label: &str) {
    for k in keyboard(symbols, shift) {
        // draw the active shift key inverted so its state is visible.
        let active = matches!(k.key, Key::Shift) && shift;
        let (fill, fg) = if active {
            (Gray4::BLACK, Gray4::WHITE)
        } else {
            (Gray4::WHITE, Gray4::BLACK)
        };
        RoundedRectangle::with_equal_corners(
            Rectangle::new(Point::new(k.x, k.y), Size::new(k.w as u32, k.h as u32)),
            Size::new(6, 6),
        )
        .into_styled(
            PrimitiveStyleBuilder::new()
                .stroke_color(Gray4::BLACK)
                .stroke_width(1)
                .fill_color(fill)
                .build(),
        )
        .draw(display)
        .ok();

        let mut buf = [0u8; 4];
        let label = key_label(k.key, symbols, enter_label, &mut buf);
        Text::with_alignment(
            label,
            Point::new(k.x + k.w / 2, k.y + k.h / 2 + 5),
            MonoTextStyle::new(&FONT_9X15, fg),
            Alignment::Center,
        )
        .draw(display)
        .ok();
    }
}

pub(crate) fn native_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(0, KB_TOP - 6, SCREEN_W, 4 * (KB_KEY_H + KB_GAP_Y) + 12)
}
