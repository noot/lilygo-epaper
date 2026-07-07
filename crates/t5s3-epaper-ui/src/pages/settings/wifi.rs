use embedded_graphics::{
    mono_font::{
        ascii::{FONT_9X15, FONT_9X18_BOLD},
        MonoTextStyle,
    },
    prelude::*,
    primitives::{PrimitiveStyle, PrimitiveStyleBuilder, Rectangle},
    text::{Alignment, Text},
};
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use t5s3_epaper_core::Display;

use super::{button, in_rect, title, BTN_H};
use crate::{
    layout::{screen_to_native_rect, SCREEN_W},
    settings::Settings,
    widgets::draw_back_button,
    wifi::ScanEntry,
};

const NETWORK_Y: i32 = 165;
const STATUS_Y: i32 = 205;
const SCAN_BTN_X: i32 = 30;
const SCAN_BTN_Y: i32 = 245;
const SCAN_BTN_W: u32 = 230;
// forces a clock sync over the saved network, doubling as an internet check.
const SYNC_BTN_X: i32 = 280;
const SYNC_BTN_W: u32 = 230;

const LIST_TOP: i32 = 345;
const ROW_H: i32 = 62;
// how many scanned networks fit on the page below the scan button.
pub(crate) const LIST_VISIBLE: usize = 9;

const PW_BOX_X: i32 = 30;
const PW_BOX_Y: i32 = 195;
const PW_BOX_W: u32 = 480;
const PW_BOX_H: u32 = 60;

pub(crate) enum Hit {
    Back,
    Scan,
    Sync,
    Network(usize),
}

fn row_y(i: usize) -> i32 {
    LIST_TOP + i as i32 * ROW_H
}

pub(crate) fn status_hit(sx: i32, sy: i32, network_count: usize) -> Option<Hit> {
    if crate::widgets::back_button_hit(sx, sy) {
        return Some(Hit::Back);
    }
    if in_rect(sx, sy, SCAN_BTN_X, SCAN_BTN_Y, SCAN_BTN_W, BTN_H) {
        return Some(Hit::Scan);
    }
    if in_rect(sx, sy, SYNC_BTN_X, SCAN_BTN_Y, SYNC_BTN_W, BTN_H) {
        return Some(Hit::Sync);
    }
    for i in 0..network_count.min(LIST_VISIBLE) {
        if in_rect(sx, sy, 30, row_y(i), (SCREEN_W - 60) as u32, ROW_H as u32) {
            return Some(Hit::Network(i));
        }
    }
    None
}

// map an RSSI (dBm) to a 0..=4 signal level for the bar indicator.
fn signal_level(rssi: i8) -> u8 {
    match rssi {
        r if r >= -55 => 4,
        r if r >= -67 => 3,
        r if r >= -78 => 2,
        r if r >= -88 => 1,
        _ => 0,
    }
}

// four ascending signal bars at (x, baseline y), `level` of them filled.
fn draw_signal(display: &mut Display, x: i32, baseline: i32, level: u8) {
    for i in 0..4u8 {
        let h = 6 + i as i32 * 6;
        let bx = x + i as i32 * 10;
        let by = baseline - h;
        let style = if i < level {
            PrimitiveStyle::with_fill(Gray4::BLACK)
        } else {
            PrimitiveStyleBuilder::new()
                .stroke_color(Gray4::new(6))
                .stroke_width(1)
                .build()
        };
        Rectangle::new(Point::new(bx, by), Size::new(7, h as u32))
            .into_styled(style)
            .draw(display)
            .ok();
    }
}

// a small padlock, drawn to the right of a secured network's name.
fn draw_lock(display: &mut Display, x: i32, y: i32) {
    // shackle (open-topped outline) above the body.
    Rectangle::new(Point::new(x + 3, y), Size::new(10, 8))
        .into_styled(
            PrimitiveStyleBuilder::new()
                .stroke_color(Gray4::BLACK)
                .stroke_width(2)
                .build(),
        )
        .draw(display)
        .ok();
    // body.
    Rectangle::new(Point::new(x, y + 6), Size::new(16, 12))
        .into_styled(PrimitiveStyle::with_fill(Gray4::BLACK))
        .draw(display)
        .ok();
}

pub(crate) fn draw_status(
    display: &mut Display,
    settings: &Settings,
    status: &str,
    networks: &[ScanEntry],
) {
    draw_back_button(display);
    title(display, "Wi-Fi");
    let configured_ssid = settings.wifi_ssid();

    let font = MonoTextStyle::new(&FONT_9X15, Gray4::BLACK);
    let mut network_line = alloc::string::String::from("Network: ");
    if configured_ssid.is_empty() {
        network_line.push_str("(not set)");
    } else {
        network_line.push_str(configured_ssid);
    }
    Text::new(&network_line, Point::new(40, NETWORK_Y), font)
        .draw(display)
        .ok();

    Text::new(
        status,
        Point::new(40, STATUS_Y),
        MonoTextStyle::new(&FONT_9X15, Gray4::new(6)),
    )
    .draw(display)
    .ok();

    button(display, SCAN_BTN_X, SCAN_BTN_Y, SCAN_BTN_W, "Scan");
    button(display, SYNC_BTN_X, SCAN_BTN_Y, SYNC_BTN_W, "Sync clock");

    for (i, entry) in networks.iter().take(LIST_VISIBLE).enumerate() {
        let y = row_y(i);
        // hairline separator so each tappable row reads as its own control.
        Rectangle::new(
            Point::new(30, y + ROW_H - 1),
            Size::new((SCREEN_W - 60) as u32, 1),
        )
        .into_styled(PrimitiveStyle::with_fill(Gray4::new(8)))
        .draw(display)
        .ok();

        // a saved network joins on tap without a password prompt; tag it so
        // that's visible in the list. keep the tag clear of the signal bars.
        let saved = settings.saved_wifi_password(&entry.ssid).is_some();
        let name = truncate(&entry.ssid, if saved { 26 } else { 34 });
        Text::with_alignment(
            name,
            Point::new(44, y + ROW_H / 2 + 5),
            MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK),
            Alignment::Left,
        )
        .draw(display)
        .ok();
        if saved {
            Text::with_alignment(
                "saved",
                Point::new(
                    44 + (name.chars().count() as i32 * 9) + 14,
                    y + ROW_H / 2 + 5,
                ),
                MonoTextStyle::new(&FONT_9X15, Gray4::new(6)),
                Alignment::Left,
            )
            .draw(display)
            .ok();
        }

        if entry.secured {
            draw_lock(display, SCREEN_W - 60, y + ROW_H / 2 - 9);
        }
        draw_signal(
            display,
            SCREEN_W - 130,
            y + ROW_H / 2 + 10,
            signal_level(entry.rssi),
        );
    }
}

// truncate on a char boundary so an arbitrary SSID never slices a codepoint.
fn truncate(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((end, _)) => &s[..end],
        None => s,
    }
}

pub(crate) fn draw_password(
    display: &mut Display,
    ssid: &str,
    password: &str,
    hint: &str,
    symbols: bool,
    shift: bool,
) {
    draw_back_button(display);
    let mut heading = alloc::string::String::from("Join ");
    heading.push_str(truncate(ssid, 24));
    title(display, &heading);

    Text::new(
        "Password:",
        Point::new(PW_BOX_X, PW_BOX_Y - 12),
        MonoTextStyle::new(&FONT_9X15, Gray4::new(6)),
    )
    .draw(display)
    .ok();
    draw_password_box(display, password);
    // status line under the box, e.g. why the keyboard reopened after a
    // failed join.
    Text::new(
        hint,
        Point::new(PW_BOX_X, PW_BOX_Y + PW_BOX_H as i32 + 30),
        MonoTextStyle::new(&FONT_9X15, Gray4::new(6)),
    )
    .draw(display)
    .ok();
    crate::keyboard::draw(display, symbols, shift, "SAVE");
}

fn draw_password_box(display: &mut Display, password: &str) {
    Rectangle::new(
        Point::new(PW_BOX_X, PW_BOX_Y),
        Size::new(PW_BOX_W, PW_BOX_H),
    )
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
    let shown = truncate(password, 50);
    Text::new(shown, Point::new(PW_BOX_X + 12, PW_BOX_Y + 36), font)
        .draw(display)
        .ok();
}

pub(crate) fn redraw_password(display: &mut Display, password: &str) {
    draw_password_box(display, password);
}

pub(crate) fn password_box_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(PW_BOX_X, PW_BOX_Y, PW_BOX_W as i32, PW_BOX_H as i32)
}
