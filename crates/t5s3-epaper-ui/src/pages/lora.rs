use alloc::{format, string::String};

use embedded_graphics::{
    mono_font::{
        ascii::{FONT_6X10, FONT_9X15, FONT_9X18_BOLD},
        MonoTextStyle,
    },
    prelude::*,
    primitives::{PrimitiveStyle, PrimitiveStyleBuilder, Rectangle},
    text::{Alignment, Text},
};
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use t5s3_epaper_core::{
    lora::{Config as LoraConfig, Lora},
    spi::Bus,
    Display,
};

use crate::{
    layout::{screen_to_native_rect, SCREEN_W},
    widgets::draw_back_button,
};

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
        "nootmesh  915 MHz",
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
    crate::keyboard::draw(display, symbols, shift, "SEND");
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

// build the lora radio used by the send/receive page. it shares SPI2 with the
// SD card via the bus, which owns and parks both chip-selects. steal the
// radio's control pins (mirroring the wifi re-sync); dropping the returned
// radio releases them. the 3.3v rail powered up at boot, so no settle delay
// is needed.
pub(crate) fn make_radio<'a>(
    bus: &'a Bus<'static>,
) -> Result<Lora<'a, 'static>, t5s3_epaper_core::lora::Error> {
    let pins = t5s3_epaper_core::lora::PinConfig {
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
