use alloc::boxed::Box;
use core::fmt::Write as _;

use embedded_graphics::{
    mono_font::{
        ascii::{FONT_9X15, FONT_9X18_BOLD},
        MonoTextStyle,
    },
    prelude::*,
    primitives::{PrimitiveStyle, PrimitiveStyleBuilder, Rectangle, RoundedRectangle},
    text::{Alignment, Text},
};
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use epub_reader::{decode_image, GrayImage};
use heapless::String as HString;
use serde::Deserialize;
use t5s3_epaper_core::Display;

use crate::{
    fmt::FmtBuf,
    layout::{screen_to_native_rect, SCREEN_W},
    widgets::{centered, draw_back_button, draw_image_fit},
};

// loop ticks (~50ms each) between live progress repaints on the music page, so
// the song position advances without a network round-trip.
pub(crate) const REFRESH_TICKS: u16 = 60;

// the progress line's screen-space band, repainted on its own for the live
// tick.
const PROGRESS_Y: i32 = 470;
const PROGRESS_BAND_Y: i32 = 452;
const PROGRESS_BAND_H: i32 = 28;

// the control-feedback status line at the bottom of the page, repainted on its
// own so a button press reports progress without blanking the page.
const STATUS_Y: i32 = 720;
const STATUS_BAND_Y: i32 = 702;
const STATUS_BAND_H: i32 = 32;

// album art box.
const ART: u32 = 260;
const ART_X: i32 = (SCREEN_W - ART as i32) / 2;
const ART_Y: i32 = 80;

// everything below the album art (track text, progress, controls, status) sits
// in this band, repainted on its own for in-place control feedback.
const BODY_TOP: i32 = 358;

// transport + volume button rows.
const BTN_H: u32 = 70;
const TRANSPORT_Y: i32 = 510;
const TRANSPORT_W: u32 = 150;
const TRANSPORT_X: [i32; 3] = [30, 195, 360];
const VOLUME_Y: i32 = 600;
const VOLUME_W: u32 = 230;
const VOLUME_X: [i32; 2] = [30, 280];

// the now-playing payload from `/api/now-playing` (the server returns `null`
// when nothing is playing, handled in `build_view`).
#[derive(Deserialize)]
pub(crate) struct NowPlaying {
    track: HString<96>,
    artist: HString<96>,
    album: HString<96>,
    is_playing: bool,
    progress_secs: Option<u32>,
    duration_secs: Option<u32>,
}

// a successful fetch: the now-playing track (None when nothing is playing) plus
// the decoded album art, if any.
pub(crate) struct Ready {
    now: Option<NowPlaying>,
    cover: Option<GrayImage>,
}

// what the music page is currently showing.
pub(crate) enum View {
    // also shown while a control is in flight.
    Loading,
    // boxed: much larger than the other variants (strings + decoded art).
    Ready(Box<Ready>),
    Error,
}

// the transport / volume controls, mapped to noot-server's POST endpoints.
#[derive(Clone, Copy)]
pub(crate) enum Button {
    Prev,
    PlayPause,
    Next,
    VolDown,
    VolUp,
}

impl Button {
    pub(crate) fn command(self) -> &'static str {
        match self {
            Self::Prev => "/now-playing/previous",
            Self::PlayPause => "/now-playing/play-pause",
            Self::Next => "/now-playing/next",
            Self::VolDown => "/now-playing/volume-down",
            Self::VolUp => "/now-playing/volume-up",
        }
    }

    // whether the command changes the below-art content (track text, progress,
    // play state); volume changes nothing on screen, so it only needs the status
    // line.
    pub(crate) fn changes_display(self) -> bool {
        !matches!(self, Self::VolUp | Self::VolDown)
    }

    // whether the command changes the album art (a track change), which needs a
    // full refresh since the grayscale art can't be partial-refreshed cleanly.
    pub(crate) fn changes_art(self) -> bool {
        matches!(self, Self::Next | Self::Prev)
    }
}

// build a view from the now-playing json and the raw (undecoded) cover bytes.
pub(crate) fn build_view(json: &[u8], cover: Option<&[u8]>) -> View {
    if json.iter().all(u8::is_ascii_whitespace) {
        return View::Error;
    }
    let cover = cover.and_then(|bytes| decode_image(bytes).ok());
    // the server sends `null` when nothing is playing.
    let nothing_playing = json
        .iter()
        .copied()
        .filter(|b| !b.is_ascii_whitespace())
        .eq(*b"null");
    if nothing_playing {
        return View::Ready(Box::new(Ready { now: None, cover }));
    }
    match serde_json_core::from_slice::<NowPlaying>(json) {
        Ok((now, _)) => View::Ready(Box::new(Ready {
            now: Some(now),
            cover,
        })),
        Err(_) => View::Error,
    }
}

// the data needed to extrapolate the playing track's position locally: where it
// was at the last fetch and how long it runs. `None` unless a track is actively
// playing with a known progress and duration (e.g. navidrome reports no
// progress, so it can't be ticked).
pub(crate) struct Playback {
    pub(crate) base_secs: u32,
    pub(crate) duration_secs: u32,
}

pub(crate) fn playback(view: &View) -> Option<Playback> {
    let View::Ready(r) = view else {
        return None;
    };
    let now = r.now.as_ref()?;
    if !now.is_playing {
        return None;
    }
    let duration_secs = now.duration_secs?;
    if duration_secs == 0 {
        return None;
    }
    Some(Playback {
        base_secs: now.progress_secs?,
        duration_secs,
    })
}

// which control button, if any, a screen-space tap landed on.
pub(crate) fn hit(sx: i32, sy: i32) -> Option<Button> {
    if (TRANSPORT_Y..TRANSPORT_Y + BTN_H as i32).contains(&sy) {
        for (x, btn) in TRANSPORT_X
            .iter()
            .zip([Button::Prev, Button::PlayPause, Button::Next])
        {
            if (*x..*x + TRANSPORT_W as i32).contains(&sx) {
                return Some(btn);
            }
        }
    }
    if (VOLUME_Y..VOLUME_Y + BTN_H as i32).contains(&sy) {
        for (x, btn) in VOLUME_X.iter().zip([Button::VolDown, Button::VolUp]) {
            if (*x..*x + VOLUME_W as i32).contains(&sx) {
                return Some(btn);
            }
        }
    }
    None
}

pub(crate) fn draw_screen(display: &mut Display, view: &View, status: Option<&str>) {
    draw_back_button(display);

    match view {
        View::Loading => {
            let label = MonoTextStyle::new(&FONT_9X15, Gray4::new(4));
            centered(display, "contacting server...", ART_Y + 140, label);
        }
        View::Error => {
            let label = MonoTextStyle::new(&FONT_9X15, Gray4::new(4));
            centered(display, "could not reach server", ART_Y + 140, label);
        }
        View::Ready(r) => {
            draw_art(display, r.cover.as_ref());
            draw_body(display, r);
        }
    }

    if let Some(status) = status {
        draw_status(display, status);
    }
}

// the track text + controls below the album art.
fn draw_body(display: &mut Display, r: &Ready) {
    let label = MonoTextStyle::new(&FONT_9X15, Gray4::new(4));
    let value = MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK);
    match &r.now {
        Some(np) => draw_track(display, np, label, value),
        None => centered(display, "nothing playing", 400, value),
    }
    let is_playing = r.now.as_ref().is_some_and(|np| np.is_playing);
    draw_controls(display, is_playing);
}

// repaint just the below-art band (track text, progress, controls, status) over
// a white fill, for in-place control feedback without a full-screen flush. the
// album art above is left untouched (it can't be partial-refreshed cleanly).
pub(crate) fn redraw_body(display: &mut Display, view: &View, status: Option<&str>) {
    Rectangle::new(
        Point::new(20, BODY_TOP),
        Size::new(
            (SCREEN_W - 40) as u32,
            (STATUS_BAND_Y + STATUS_BAND_H - BODY_TOP) as u32,
        ),
    )
    .into_styled(PrimitiveStyle::with_fill(Gray4::WHITE))
    .draw(display)
    .ok();
    if let View::Ready(r) = view {
        draw_body(display, r);
    }
    if let Some(status) = status {
        draw_status(display, status);
    }
}

pub(crate) fn below_art_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(
        20,
        BODY_TOP,
        SCREEN_W - 40,
        STATUS_BAND_Y + STATUS_BAND_H - BODY_TOP,
    )
}

// repaint the bottom status line over a white fill. pure black so it survives
// the 1-bit fast partial refresh used for in-place control feedback.
pub(crate) fn draw_status(display: &mut Display, text: &str) {
    Rectangle::new(
        Point::new(20, STATUS_BAND_Y),
        Size::new((SCREEN_W - 40) as u32, STATUS_BAND_H as u32),
    )
    .into_styled(PrimitiveStyle::with_fill(Gray4::WHITE))
    .draw(display)
    .ok();
    centered(
        display,
        text,
        STATUS_Y,
        MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK),
    );
}

pub(crate) fn status_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(20, STATUS_BAND_Y, SCREEN_W - 40, STATUS_BAND_H)
}

fn draw_art(display: &mut Display, cover: Option<&GrayImage>) {
    match cover {
        Some(img) => draw_image_fit(display, img, ART_X, ART_Y, ART, ART),
        None => {
            centered(
                display,
                "no art",
                ART_Y + ART as i32 / 2,
                MonoTextStyle::new(&FONT_9X15, Gray4::new(8)),
            );
        }
    }
    // frame the art box.
    Rectangle::new(Point::new(ART_X, ART_Y), Size::new(ART, ART))
        .into_styled(
            PrimitiveStyleBuilder::new()
                .stroke_color(Gray4::BLACK)
                .stroke_width(2)
                .build(),
        )
        .draw(display)
        .ok();
}

fn draw_track(
    display: &mut Display,
    np: &NowPlaying,
    label: MonoTextStyle<'_, Gray4>,
    value: MonoTextStyle<'_, Gray4>,
) {
    centered(display, np.track.as_str(), 380, value);
    centered(display, np.artist.as_str(), 410, label);
    centered(display, np.album.as_str(), 436, label);
    draw_progress_line(display, np.is_playing, np.progress_secs, np.duration_secs);
}

// repaint just the progress line for the given position. used both in the full
// page draw and by the periodic live tick, so it fills its band white first.
pub(crate) fn draw_progress(display: &mut Display, current_secs: u32, duration_secs: u32) {
    draw_progress_line(display, true, Some(current_secs), Some(duration_secs));
}

fn draw_progress_line(
    display: &mut Display,
    is_playing: bool,
    progress: Option<u32>,
    duration: Option<u32>,
) {
    Rectangle::new(
        Point::new(20, PROGRESS_BAND_Y),
        Size::new((SCREEN_W - 40) as u32, PROGRESS_BAND_H as u32),
    )
    .into_styled(PrimitiveStyle::with_fill(Gray4::WHITE))
    .draw(display)
    .ok();

    let mut line = FmtBuf::<32>::new();
    write!(line, "{}", if is_playing { "playing" } else { "paused" }).ok();
    if let (Some(p), Some(d)) = (progress, duration) {
        write!(
            line,
            "   {}:{:02} / {}:{:02}",
            p / 60,
            p % 60,
            d / 60,
            d % 60
        )
        .ok();
    }
    // pure black: the live tick repaints this band with the 1-bit fast waveform.
    centered(
        display,
        line.as_str(),
        PROGRESS_Y,
        MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK),
    );
}

pub(crate) fn progress_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(20, PROGRESS_BAND_Y, SCREEN_W - 40, PROGRESS_BAND_H)
}

fn draw_controls(display: &mut Display, is_playing: bool) {
    let labels = [
        (TRANSPORT_X[0], TRANSPORT_W, TRANSPORT_Y, "|<<"),
        (
            TRANSPORT_X[1],
            TRANSPORT_W,
            TRANSPORT_Y,
            if is_playing { "Pause" } else { "Play" },
        ),
        (TRANSPORT_X[2], TRANSPORT_W, TRANSPORT_Y, ">>|"),
        (VOLUME_X[0], VOLUME_W, VOLUME_Y, "Vol -"),
        (VOLUME_X[1], VOLUME_W, VOLUME_Y, "Vol +"),
    ];
    for (x, w, y, text) in labels {
        draw_button(display, x, y, w, text);
    }
}

fn draw_button(display: &mut Display, x: i32, y: i32, w: u32, text: &str) {
    let border = PrimitiveStyleBuilder::new()
        .stroke_color(Gray4::BLACK)
        .stroke_width(3)
        .fill_color(Gray4::WHITE)
        .build();
    RoundedRectangle::with_equal_corners(
        Rectangle::new(Point::new(x, y), Size::new(w, BTN_H)),
        Size::new(12, 12),
    )
    .into_styled(border)
    .draw(display)
    .ok();
    Text::with_alignment(
        text,
        Point::new(x + w as i32 / 2, y + BTN_H as i32 / 2 + 6),
        MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK),
        Alignment::Center,
    )
    .draw(display)
    .ok();
}
