use alloc::boxed::Box;
use core::fmt::Write as _;

use embedded_graphics::{
    mono_font::{
        ascii::{FONT_9X15, FONT_9X18_BOLD},
        MonoTextStyle,
    },
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle},
    text::{Alignment, Text},
};
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use heapless::{String as HString, Vec as HVec};
use serde::Deserialize;
use t5s3_epaper_core::Display;

use crate::{
    fmt::FmtBuf,
    layout::SCREEN_W,
    widgets::{centered, draw_back_button},
};

// the most sensor devices the page parses from one response; one card each.
const MAX_SENSORS: usize = 8;

const CARDS_TOP: i32 = 176;
const CARD_H: i32 = 150;
// how many cards fit below the header before running off the 960px-tall screen.
const MAX_VISIBLE: usize = 5;

// a single device's latest reading from `/api/sensors`. every metric field is
// optional so one struct covers both sensor kinds: air sensors report co2 and
// humidity, water sensors report tds_ppm. received_at is present in the body
// but ignored here.
#[derive(Deserialize)]
pub(crate) struct Reading {
    id: HString<32>,
    co2: Option<u16>,
    tds_ppm: Option<f32>,
    temperature_c: f32,
    humidity: Option<f32>,
}

// what the environment page is currently showing. Ready is boxed: the readings
// list is much larger than the other variants.
pub(crate) enum View {
    Loading,
    Ready(Box<HVec<Reading, MAX_SENSORS>>),
    Error,
}

// the request path for the list of all sensor devices.
pub(crate) fn path() -> &'static str {
    "/api/sensors"
}

// parse an `/api/sensors` list response body into a view.
pub(crate) fn parse(body: &[u8]) -> View {
    match serde_json_core::from_slice::<HVec<Reading, MAX_SENSORS>>(body) {
        Ok((readings, _)) => View::Ready(Box::new(readings)),
        Err(_) => View::Error,
    }
}

// a word describing air quality at a given co2 level, matching the server's
// sensor dashboard.
fn quality(co2: u16) -> &'static str {
    match co2 {
        0..=800 => "fresh",
        801..=1000 => "good",
        1001..=1400 => "fair",
        1401..=2000 => "stuffy",
        _ => "poor",
    }
}

// a word describing water hardness at a given tds level, matching the server's
// sensor dashboard.
fn hardness(tds_ppm: f32) -> &'static str {
    if tds_ppm < 100.0 {
        "very soft"
    } else if tds_ppm < 250.0 {
        "soft"
    } else if tds_ppm < 450.0 {
        "moderate"
    } else if tds_ppm < 700.0 {
        "hard"
    } else {
        "very hard"
    }
}

pub(crate) fn draw_screen(display: &mut Display, view: &View) {
    let bold = MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK);
    draw_back_button(display);
    Text::with_alignment(
        "Environment",
        Point::new(SCREEN_W / 2, 120),
        bold,
        Alignment::Center,
    )
    .draw(display)
    .ok();
    draw_body(display, view);
}

// the card area, drawn over a white fill so a refresh cleanly replaces it.
fn draw_body(display: &mut Display, view: &View) {
    Rectangle::new(Point::new(16, CARDS_TOP - 12), Size::new(508, 784))
        .into_styled(PrimitiveStyle::with_fill(Gray4::WHITE))
        .draw(display)
        .ok();

    let label = MonoTextStyle::new(&FONT_9X15, Gray4::new(4));

    match view {
        View::Loading => centered(display, "loading...", CARDS_TOP + 120, label),
        View::Error => centered(display, "no readings available", CARDS_TOP + 120, label),
        View::Ready(readings) if readings.is_empty() => {
            centered(display, "no sensors reporting", CARDS_TOP + 120, label)
        }
        View::Ready(readings) => {
            let shown = readings.len().min(MAX_VISIBLE);
            for (i, reading) in readings.iter().take(shown).enumerate() {
                draw_card(display, reading, CARDS_TOP + i as i32 * CARD_H);
            }
            if readings.len() > shown {
                let mut more = FmtBuf::<24>::new();
                write!(more, "+{} more", readings.len() - shown).ok();
                centered(
                    display,
                    more.as_str(),
                    CARDS_TOP + shown as i32 * CARD_H + 18,
                    label,
                );
            }
        }
    }
}

// one sensor's card: device id and kind, its headline metric with a quality
// word, and the secondary values, above a divider.
fn draw_card(display: &mut Display, reading: &Reading, top: i32) {
    let bold = MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK);
    let label = MonoTextStyle::new(&FONT_9X15, Gray4::new(4));

    Text::new(reading.id.as_str(), Point::new(40, top + 26), bold)
        .draw(display)
        .ok();
    let kind = if reading.co2.is_some() {
        "air"
    } else if reading.tds_ppm.is_some() {
        "water"
    } else {
        "sensor"
    };
    Text::with_alignment(kind, Point::new(500, top + 26), label, Alignment::Right)
        .draw(display)
        .ok();

    let mut primary = FmtBuf::<40>::new();
    let mut secondary = FmtBuf::<32>::new();
    match (reading.co2, reading.tds_ppm) {
        (Some(co2), _) => {
            write!(primary, "CO2   {co2} ppm ({})", quality(co2)).ok();
            write!(secondary, "{:.1} C", reading.temperature_c).ok();
            if let Some(humidity) = reading.humidity {
                write!(secondary, "     {humidity:.0} %RH").ok();
            }
        }
        (None, Some(tds)) => {
            write!(primary, "TDS   {tds:.0} ppm ({})", hardness(tds)).ok();
            write!(secondary, "{:.1} C water", reading.temperature_c).ok();
        }
        (None, None) => {
            write!(primary, "{:.1} C", reading.temperature_c).ok();
        }
    }
    Text::new(primary.as_str(), Point::new(40, top + 66), bold)
        .draw(display)
        .ok();
    Text::new(secondary.as_str(), Point::new(40, top + 98), label)
        .draw(display)
        .ok();

    Rectangle::new(
        Point::new(30, top + CARD_H - 18),
        Size::new((SCREEN_W - 60) as u32, 2),
    )
    .into_styled(PrimitiveStyle::with_fill(Gray4::new(8)))
    .draw(display)
    .ok();
}
