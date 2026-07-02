use alloc::{format, string::String, vec::Vec};
use core::cell::RefCell;

use embedded_graphics::{
    mono_font::{
        ascii::{FONT_6X10, FONT_9X15, FONT_9X18_BOLD},
        MonoTextStyle,
    },
    prelude::*,
    primitives::{PrimitiveStyle, PrimitiveStyleBuilder, Rectangle, RoundedRectangle},
    text::{Alignment, Text},
};
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use esp_hal::{spi::master::Spi, Blocking};
use t5s3_epaper_core::{
    lora::{Config as LoraConfig, Lora},
    Display,
};

use crate::{
    layout::{screen_to_native_rect, SCREEN_W},
    widgets::draw_back_button,
};

// on-screen touch keyboard for composing a message to broadcast. the keys are
// static, so the whole board is painted once on entry (full refresh); only the
// message box / status line / keyboard repaint as you type (partial refresh).
const KB_KEY_W: i32 = 50;
const KB_KEY_H: i32 = 78;
const KB_GAP: i32 = 4;
const KB_GAP_Y: i32 = 8;
// keyboard sits at the bottom of the screen (rows end ~24px from the edge).
const KB_TOP: i32 = 600;
const KB_X: i32 = 2;
const KB_FULL_W: i32 = 536;
const KB_TOGGLE_W: i32 = 90;
const KB_SEND_W: i32 = 110;

const MSG_X: i32 = 30;
const MSG_Y: i32 = 150;
const MSG_W: u32 = 480;
const MSG_H: u32 = 170;
const LORA_STATUS_Y: i32 = 338;
pub(crate) const MSG_MAX: usize = 200;

// sent + received message logs, stacked between the status line and keyboard.
pub(crate) const SENT_Y: i32 = 368;
pub(crate) const RECV_Y: i32 = 476;
const LIST_H: u32 = 102;
pub(crate) const LIST_MAX: usize = 3;

const KB_LETTERS: [&str; 3] = ["qwertyuiop", "asdfghjkl", "zxcvbnm"];
const KB_SYMBOLS: [&str; 3] = ["1234567890", "@#$&-+()/", "*\"':;!?,"];

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Key {
    Char(char),
    Shift,
    Symbols,
    Backspace,
    Space,
    Send,
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

    // bottom row: layer toggle, wide space bar, send.
    let y = kb_row_y(3);
    keys.push(KeyBox {
        key: Key::Symbols,
        x: KB_X,
        y,
        w: KB_TOGGLE_W,
        h: KB_KEY_H,
    });
    let send_x = KB_X + KB_FULL_W - KB_SEND_W;
    let space_x = KB_X + KB_TOGGLE_W + KB_GAP;
    keys.push(KeyBox {
        key: Key::Space,
        x: space_x,
        y,
        w: send_x - KB_GAP - space_x,
        h: KB_KEY_H,
    });
    keys.push(KeyBox {
        key: Key::Send,
        x: send_x,
        y,
        w: KB_SEND_W,
        h: KB_KEY_H,
    });

    keys
}

pub(crate) fn kb_hit(sx: i32, sy: i32, symbols: bool, shift: bool) -> Option<Key> {
    keyboard(symbols, shift)
        .into_iter()
        .find_map(|k| (sx >= k.x && sx < k.x + k.w && sy >= k.y && sy < k.y + k.h).then_some(k.key))
}

fn key_label(key: Key, symbols: bool, buf: &mut [u8; 4]) -> &str {
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
        Key::Send => "SEND",
    }
}

pub(crate) fn draw_keyboard(display: &mut Display, symbols: bool, shift: bool) {
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
        let label = key_label(k.key, symbols, &mut buf);
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

pub(crate) fn draw_message(display: &mut Display, message: &str) {
    Rectangle::new(Point::new(MSG_X, MSG_Y), Size::new(MSG_W, MSG_H))
        .into_styled(
            PrimitiveStyleBuilder::new()
                .stroke_color(Gray4::BLACK)
                .stroke_width(2)
                .fill_color(Gray4::WHITE)
                .build(),
        )
        .draw(display)
        .ok();

    let font = MonoTextStyle::new(&FONT_9X15, Gray4::BLACK);
    let x = MSG_X + 12;
    let mut y = MSG_Y + 28;
    if message.is_empty() {
        Text::new("type a message...", Point::new(x, y), font)
            .draw(display)
            .ok();
        return;
    }

    // wrap on a character count; the font is fixed width and the text is ascii.
    let per_line = ((MSG_W as i32 - 24) / 9) as usize;
    let bytes = message.len();
    let mut start = 0;
    while start < bytes {
        let end = (start + per_line).min(bytes);
        Text::new(&message[start..end], Point::new(x, y), font)
            .draw(display)
            .ok();
        y += 20;
        start = end;
    }
}

pub(crate) fn draw_lora_status(display: &mut Display, status: &str) {
    Rectangle::new(Point::new(MSG_X, LORA_STATUS_Y), Size::new(MSG_W, 26))
        .into_styled(PrimitiveStyle::with_fill(Gray4::WHITE))
        .draw(display)
        .ok();
    Text::with_alignment(
        status,
        Point::new(SCREEN_W / 2, LORA_STATUS_Y + 18),
        MonoTextStyle::new(&FONT_9X15, Gray4::BLACK),
        Alignment::Center,
    )
    .draw(display)
    .ok();
}

// a titled message log (newest first), each entry truncated to one line. used
// for both the sent and received lists; `y` is the top of its section.
pub(crate) fn draw_list(display: &mut Display, y: i32, header: &str, items: &[String]) {
    Rectangle::new(Point::new(MSG_X, y), Size::new(MSG_W, LIST_H))
        .into_styled(PrimitiveStyle::with_fill(Gray4::WHITE))
        .draw(display)
        .ok();
    Text::new(
        header,
        Point::new(MSG_X + 4, y + 16),
        MonoTextStyle::new(&FONT_9X15, Gray4::BLACK),
    )
    .draw(display)
    .ok();

    let font = MonoTextStyle::new(&FONT_6X10, Gray4::BLACK);
    let mut ey = y + 40;
    for msg in items.iter().rev() {
        // truncate on a char boundary, not a byte index: received messages are
        // arbitrary utf-8 from a peer, so slicing at a fixed byte would panic
        // mid-codepoint.
        let line = match msg.char_indices().nth(66) {
            Some((end, _)) => format!("> {}...", &msg[..end]),
            None => format!("> {msg}"),
        };
        Text::new(&line, Point::new(MSG_X + 10, ey), font)
            .draw(display)
            .ok();
        ey += 20;
    }
}

pub(crate) fn draw_lora_screen(
    display: &mut Display,
    message: &str,
    status: &str,
    sent: &[String],
    received: &[String],
    symbols: bool,
    shift: bool,
) {
    let bold = MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK);
    draw_back_button(display);
    Text::with_alignment(
        "LoRa  915 MHz",
        Point::new(SCREEN_W / 2, 120),
        bold,
        Alignment::Center,
    )
    .draw(display)
    .ok();
    draw_message(display, message);
    draw_lora_status(display, status);
    draw_list(display, SENT_Y, "sent", sent);
    draw_list(display, RECV_Y, "received", received);
    draw_keyboard(display, symbols, shift);
}

pub(crate) fn message_box_native_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(MSG_X, MSG_Y, MSG_W as i32, MSG_H as i32)
}

pub(crate) fn lora_status_native_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(MSG_X, LORA_STATUS_Y, MSG_W as i32, 26)
}

pub(crate) fn sent_native_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(MSG_X, SENT_Y, MSG_W as i32, LIST_H as i32)
}

pub(crate) fn received_native_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(MSG_X, RECV_Y, MSG_W as i32, LIST_H as i32)
}

pub(crate) fn keyboard_native_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(0, KB_TOP - 6, SCREEN_W, 4 * (KB_KEY_H + KB_GAP_Y) + 12)
}

// build the lora radio used by the send/receive page. it shares SPI2 with the
// SD card, which is only touched at sleep (after the main loop), so the bus is
// free while this page is open. steal the bus + radio pins (mirroring the wifi
// re-sync); dropping the returned radio releases them. the 3.3v rail powered up
// at boot, so no settle delay is needed.
pub(crate) fn make_radio<'a>(
    bus: &'a RefCell<Spi<'static, Blocking>>,
) -> Result<Lora<'a, 'static>, t5s3_epaper_core::lora::Error> {
    let pins = t5s3_epaper_core::lora::PinConfig {
        cs: unsafe { esp_hal::peripherals::GPIO46::steal() },
        rst: unsafe { esp_hal::peripherals::GPIO1::steal() },
        busy: unsafe { esp_hal::peripherals::GPIO47::steal() },
        dio1: unsafe { esp_hal::peripherals::GPIO10::steal() },
    };
    // match the t3-s3 receiver, whose Config::default() uses SF7. every other
    // parameter (915 MHz, BW125, CR4/5, preamble 8, private sync word) already
    // agrees; only the spreading factor differed.
    let config = LoraConfig {
        spreading_factor: t5s3_epaper_core::lora::SpreadingFactor::Sf7,
        ..LoraConfig::default()
    };
    Lora::new(bus, pins, &config)
}
