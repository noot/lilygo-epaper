use alloc::vec::Vec;

use embedded_graphics::{
    mono_font::{ascii::FONT_9X15, MonoTextStyle},
    prelude::*,
    primitives::{PrimitiveStyle, PrimitiveStyleBuilder, Rectangle, RoundedRectangle, Triangle},
    text::{Alignment, Text},
};
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use t5s3_epaper_core::Display;
use u8g2_fonts::{
    fonts,
    types::{FontColor, HorizontalAlignment, VerticalPosition},
    FontRenderer,
};

use crate::layout::{screen_to_native_rect, SAFE_W, SAFE_X, SCREEN_W};

// on-screen touch keyboard shared by the lora composer, the wifi password
// entry, the notes editor and the mesh alias editor. the keys are static, so
// the whole board is painted once on entry (full refresh); callers repaint
// just their input field on each keystroke (partial refresh). the enter
// key's label is caller-supplied ("SEND", "SAVE", ...).
const KB_KEY_W: i32 = 47;
const KB_KEY_H: i32 = 78;
const KB_GAP: i32 = 4;
const KB_GAP_Y: i32 = 8;
// keyboard sits at the bottom of the screen (rows end ~24px from the edge).
const KB_TOP: i32 = 600;
// inset to the same case-safe strip as the rest of the touch grid, so a
// rounded key corner in the outer columns (q, p) doesn't sit under the
// bezel.
const KB_X: i32 = SAFE_X;
const KB_FULL_W: i32 = SAFE_W;
const KB_TOGGLE_W: i32 = 90;
const KB_ENTER_W: i32 = 110;

// twice the cap height of the FONT_9X15 label these replaced, for a letter
// key that's actually legible at arm's length on the composer/editor pages.
static CHAR_FONT: FontRenderer = FontRenderer::new::<fonts::u8g2_font_fub25_tf>();

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
        let ox = KB_X + (KB_FULL_W - (n * KB_KEY_W + (n - 1) * KB_GAP)) / 2;
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
    let ox = KB_X + (KB_FULL_W - (9 * KB_KEY_W + 8 * KB_GAP)) / 2;
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

fn draw_label(display: &mut Display, cx: i32, cy: i32, label: &str, color: Gray4) {
    Text::with_alignment(
        label,
        Point::new(cx, cy + 5),
        MonoTextStyle::new(&FONT_9X15, color),
        Alignment::Center,
    )
    .draw(display)
    .ok();
}

// upward arrow: the standard shift glyph. drawn instead of a text label so
// it stays legible at the key's small footprint.
fn draw_shift_icon(display: &mut Display, cx: i32, cy: i32, color: Gray4) {
    let style = PrimitiveStyle::with_fill(color);
    Triangle::new(
        Point::new(cx, cy - 12),
        Point::new(cx - 11, cy + 2),
        Point::new(cx + 11, cy + 2),
    )
    .into_styled(style)
    .draw(display)
    .ok();
    Rectangle::new(Point::new(cx - 5, cy + 2), Size::new(10, 10))
        .into_styled(style)
        .draw(display)
        .ok();
}

// leftward arrow: reads as "backspace" (delete-back-one), not the
// delete-forward the old "del" text label implied.
fn draw_backspace_icon(display: &mut Display, cx: i32, cy: i32, color: Gray4) {
    let style = PrimitiveStyle::with_fill(color);
    Triangle::new(
        Point::new(cx - 13, cy),
        Point::new(cx - 1, cy - 11),
        Point::new(cx - 1, cy + 11),
    )
    .into_styled(style)
    .draw(display)
    .ok();
    Rectangle::new(Point::new(cx - 1, cy - 6), Size::new(15, 12))
        .into_styled(style)
        .draw(display)
        .ok();
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

        let cx = k.x + k.w / 2;
        let cy = k.y + k.h / 2;
        match k.key {
            Key::Shift => draw_shift_icon(display, cx, cy, fg),
            Key::Backspace => draw_backspace_icon(display, cx, cy, fg),
            Key::Char(c) => {
                let mut buf = [0u8; 4];
                CHAR_FONT
                    .render_aligned(
                        c.encode_utf8(&mut buf) as &str,
                        Point::new(cx, cy),
                        VerticalPosition::Center,
                        HorizontalAlignment::Center,
                        FontColor::Transparent(fg),
                        display,
                    )
                    .ok();
            }
            Key::Symbols if symbols => draw_label(display, cx, cy, "abc", fg),
            Key::Symbols => draw_label(display, cx, cy, "123", fg),
            Key::Space => draw_label(display, cx, cy, "space", fg),
            Key::Enter => draw_label(display, cx, cy, enter_label, fg),
        }
    }
}

pub(crate) fn native_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(0, KB_TOP - 6, SCREEN_W, 4 * (KB_KEY_H + KB_GAP_Y) + 12)
}
