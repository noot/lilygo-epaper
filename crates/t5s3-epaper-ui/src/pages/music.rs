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
use heapless::String as HString;
use serde::Deserialize;
use t5s3_epaper_core::Display;

use crate::{fmt::FmtBuf, layout::SCREEN_W, widgets::draw_back_button};

// the now-playing path on noot-server.
pub(crate) const PATH: &str = "/api/now-playing";

const BODY_TOP: i32 = 200;
const BODY_H: u32 = 360;

// the now-playing payload from `/api/now-playing` (the server returns `null`
// when nothing is playing, handled in `parse`).
#[derive(Deserialize)]
pub(crate) struct NowPlaying {
    track: HString<96>,
    artist: HString<96>,
    album: HString<96>,
    is_playing: bool,
    progress_secs: Option<u32>,
    duration_secs: Option<u32>,
}

// what the music page is currently showing.
pub(crate) enum View {
    Loading,
    // boxed: the parsed payload is much larger than the other variants.
    Playing(Box<NowPlaying>),
    Idle,
    Error,
}

// parse a `/api/now-playing` response body into a view.
pub(crate) fn parse(body: &[u8]) -> View {
    if body.iter().all(u8::is_ascii_whitespace) {
        return View::Error;
    }
    if body
        .iter()
        .copied()
        .filter(|b| !b.is_ascii_whitespace())
        .eq(*b"null")
    {
        return View::Idle;
    }
    match serde_json_core::from_slice::<NowPlaying>(body) {
        Ok((np, _)) => View::Playing(Box::new(np)),
        Err(_) => View::Error,
    }
}

// "1:23" for a count of seconds.
fn write_clock(buf: &mut FmtBuf<8>, secs: u32) {
    write!(buf, "{}:{:02}", secs / 60, secs % 60).ok();
}

pub(crate) fn draw_screen(display: &mut Display, view: &View) {
    let bold = MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK);
    draw_back_button(display);
    Text::with_alignment(
        "Music",
        Point::new(SCREEN_W / 2, 120),
        bold,
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
        View::Idle => {
            centered(display, "nothing playing", BODY_TOP + 120, value);
        }
        View::Error => {
            centered(display, "could not reach server", BODY_TOP + 120, label);
        }
        View::Playing(np) => {
            let rows = [
                ("Track", np.track.as_str()),
                ("Artist", np.artist.as_str()),
                ("Album", np.album.as_str()),
            ];
            let mut y = BODY_TOP + 40;
            for (name, val) in rows {
                Text::new(name, Point::new(40, y), label).draw(display).ok();
                Text::new(val, Point::new(40, y + 26), value)
                    .draw(display)
                    .ok();
                y += 80;
            }

            // state + progress line, e.g. "playing   1:23 / 3:45".
            let mut line = FmtBuf::<24>::new();
            write!(line, "{}", if np.is_playing { "playing" } else { "paused" }).ok();
            if let (Some(p), Some(d)) = (np.progress_secs, np.duration_secs) {
                let mut prog = FmtBuf::<8>::new();
                let mut dur = FmtBuf::<8>::new();
                write_clock(&mut prog, p);
                write_clock(&mut dur, d);
                write!(line, "   {} / {}", prog.as_str(), dur.as_str()).ok();
            }
            Text::new(line.as_str(), Point::new(40, y + 10), label)
                .draw(display)
                .ok();
        }
    }
}

fn centered(display: &mut Display, text: &str, y: i32, style: MonoTextStyle<'_, Gray4>) {
    Text::with_alignment(text, Point::new(SCREEN_W / 2, y), style, Alignment::Center)
        .draw(display)
        .ok();
}
