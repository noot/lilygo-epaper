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
use serde::Deserialize;
use t5s3_epaper_core::Display;

use crate::{fmt::FmtBuf, layout::SCREEN_W, widgets::draw_back_button};

// the sensor device whose latest reading this page shows (from .env at build
// time; see the justfile). matches the id the sensor firmware posts under.
const SENSOR_ID: &str = match option_env!("SENSOR_ID") {
    Some(s) => s,
    None => "living-room",
};

const BODY_TOP: i32 = 200;
const BODY_H: u32 = 360;

// a single sensor sample from `/api/sensors/{id}`. received_at is ignored here.
#[derive(Deserialize)]
pub(crate) struct Reading {
    co2: u16,
    temperature_c: f32,
    humidity: f32,
}

// what the environment page is currently showing.
pub(crate) enum View {
    Loading,
    Ready(Reading),
    Error,
}

// the request path for the configured sensor device.
pub(crate) fn path() -> FmtBuf<48> {
    let mut buf = FmtBuf::<48>::new();
    write!(buf, "/api/sensors/{SENSOR_ID}").ok();
    buf
}

// parse a `/api/sensors/{id}` response body into a view.
pub(crate) fn parse(body: &[u8]) -> View {
    match serde_json_core::from_slice::<Reading>(body) {
        Ok((reading, _)) => View::Ready(reading),
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
    Text::with_alignment(
        SENSOR_ID,
        Point::new(SCREEN_W / 2, 160),
        MonoTextStyle::new(&FONT_9X15, Gray4::new(4)),
        Alignment::Center,
    )
    .draw(display)
    .ok();
    draw_body(display, view);
}

// the data area, drawn over a white fill so a refresh cleanly replaces it.
fn draw_body(display: &mut Display, view: &View) {
    Rectangle::new(Point::new(20, BODY_TOP), Size::new(500, BODY_H))
        .into_styled(PrimitiveStyle::with_fill(Gray4::WHITE))
        .draw(display)
        .ok();

    let label = MonoTextStyle::new(&FONT_9X15, Gray4::new(4));
    let value = MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK);

    match view {
        View::Loading => {
            centered(display, "loading...", BODY_TOP + 120, label);
        }
        View::Error => {
            centered(display, "no reading available", BODY_TOP + 120, label);
        }
        View::Ready(r) => {
            let mut co2 = FmtBuf::<24>::new();
            write!(co2, "{} ppm ({})", r.co2, quality(r.co2)).ok();
            let mut temp = FmtBuf::<16>::new();
            write!(temp, "{:.1} C", r.temperature_c).ok();
            let mut hum = FmtBuf::<16>::new();
            write!(hum, "{:.0} %RH", r.humidity).ok();

            let rows = [
                ("CO2", co2.as_str()),
                ("Temperature", temp.as_str()),
                ("Humidity", hum.as_str()),
            ];
            let mut y = BODY_TOP + 40;
            for (name, val) in rows {
                Text::new(name, Point::new(40, y), label).draw(display).ok();
                Text::new(val, Point::new(40, y + 26), value)
                    .draw(display)
                    .ok();
                y += 80;
            }
        }
    }
}

fn centered(display: &mut Display, text: &str, y: i32, style: MonoTextStyle<'_, Gray4>) {
    Text::with_alignment(text, Point::new(SCREEN_W / 2, y), style, Alignment::Center)
        .draw(display)
        .ok();
}
