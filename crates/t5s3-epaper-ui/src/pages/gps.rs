use core::fmt::Write as _;

use embedded_graphics::{
    mono_font::{ascii::FONT_6X10, MonoTextStyle},
    prelude::*,
    primitives::{Circle, PrimitiveStyle, Rectangle},
    text::{Alignment, Text},
};
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use epub_reader::{decode_image, GrayImage};
use t5s3_epaper_core::{gps::Gps, Display};

use crate::{fmt::FmtBuf, layout::screen_to_native_rect, widgets::draw_image_fit};

// loop ticks between GPS readout refreshes (~50ms per tick)
pub(crate) const GPS_REFRESH_TICKS: u16 = 30;

// a position fix worth keeping on screen after the live fix drops, so a brief
// signal loss shows the last known position instead of blanking to "--".
#[derive(Clone, Copy)]
pub(crate) struct GpsFix {
    lat: f64,
    lng: f64,
    alt: f32,
    speed: f32,
    hdop: f32,
    vdop: f32,
    sats: u32,
}

impl GpsFix {
    pub(crate) fn lat(&self) -> f64 {
        self.lat
    }

    pub(crate) fn lon(&self) -> f64 {
        self.lng
    }
}

// snapshot the current fix, or None if the receiver has no position right now.
pub(crate) fn current_fix(gps: &Gps<'_>) -> Option<GpsFix> {
    let (lat, lng) = gps.location()?;
    Some(GpsFix {
        lat,
        lng,
        alt: gps.altitude().unwrap_or(0.0),
        speed: gps.speed_over_ground().unwrap_or(0.0),
        hdop: gps.hdop().unwrap_or(0.0),
        vdop: gps.vdop().unwrap_or(0.0),
        sats: gps.fix_satellites().unwrap_or(0),
    })
}

pub(crate) fn draw_gps_data(display: &mut Display, gps: &Gps<'_>, last_fix: Option<GpsFix>) {
    let small = MonoTextStyle::new(&FONT_6X10, Gray4::BLACK);
    let x = 60;
    let line_h = 28;
    let mut y = 200;

    // fill the data area with white before drawing text, matching the
    // u8g2 WithBackground pattern used in the working gps example.
    // this ensures previous text is explicitly cleared in the framebuffer
    // so the DU waveform sees clean transitions.
    Rectangle::new(Point::new(30, 170), Size::new(480, 300))
        .into_styled(PrimitiveStyle::with_fill(Gray4::WHITE))
        .draw(display)
        .ok();

    let module_name = match gps.module() {
        t5s3_epaper_core::gps::Module::L76K => "L76K",
        t5s3_epaper_core::gps::Module::MiaM10Q => "MIA-M10Q",
    };

    let in_view = gps.satellites_in_view();
    let current = current_fix(gps);
    // when the live fix drops, keep showing the last known position rather than
    // blanking it. the DU partial-refresh waveform is 1-bit, so we flag the
    // stale data with text ("re-acquiring" / "(last)") rather than a grey tone
    // it can't render.
    let stale = current.is_none() && last_fix.is_some();
    let shown = current.or(last_fix);
    let mark = if stale { "  (last)" } else { "" };

    let mut buf = FmtBuf::<48>::new();

    write!(buf, "Module: {}", module_name).ok();
    Text::new(buf.as_str(), Point::new(x, y), small)
        .draw(display)
        .ok();
    y += line_h;

    buf.reset();
    match current {
        Some(f) => {
            let fix_str = match gps.fix_type() {
                Some(nmea::sentences::FixType::Gps) => "GPS",
                Some(nmea::sentences::FixType::DGps) => "DGPS",
                Some(nmea::sentences::FixType::Rtk) => "RTK",
                Some(nmea::sentences::FixType::FloatRtk) => "float RTK",
                Some(_) => "other",
                None => "fix",
            };
            write!(
                buf,
                "Fix:    {} ({} used, {} in view)",
                fix_str, f.sats, in_view
            )
            .ok();
        }
        None if stale => {
            write!(buf, "Fix:    re-acquiring ({in_view} in view)").ok();
        }
        None => {
            write!(buf, "Fix:    no fix ({in_view} in view)").ok();
        }
    }
    Text::new(buf.as_str(), Point::new(x, y), small)
        .draw(display)
        .ok();
    y += line_h + 10;

    match shown {
        Some(f) => {
            buf.reset();
            write!(buf, "Lat:    {:.6}{}", f.lat, mark).ok();
            Text::new(buf.as_str(), Point::new(x, y), small)
                .draw(display)
                .ok();
            y += line_h;

            buf.reset();
            write!(buf, "Lon:    {:.6}{}", f.lng, mark).ok();
            Text::new(buf.as_str(), Point::new(x, y), small)
                .draw(display)
                .ok();
            y += line_h;

            buf.reset();
            write!(buf, "Alt:    {:.1} m", f.alt).ok();
            Text::new(buf.as_str(), Point::new(x, y), small)
                .draw(display)
                .ok();
            y += line_h;

            buf.reset();
            write!(buf, "Speed:  {:.1} kn", f.speed).ok();
            Text::new(buf.as_str(), Point::new(x, y), small)
                .draw(display)
                .ok();
            y += line_h;

            buf.reset();
            write!(buf, "HDOP:   {:.1}   VDOP: {:.1}", f.hdop, f.vdop).ok();
            Text::new(buf.as_str(), Point::new(x, y), small)
                .draw(display)
                .ok();
        }
        None => {
            Text::new("Lat:    --", Point::new(x, y), small)
                .draw(display)
                .ok();
            y += line_h;
            Text::new("Lon:    --", Point::new(x, y), small)
                .draw(display)
                .ok();
            y += line_h;
            Text::new("Alt:    --", Point::new(x, y), small)
                .draw(display)
                .ok();
            y += line_h;
            Text::new("Speed:  --", Point::new(x, y), small)
                .draw(display)
                .ok();
            y += line_h;
            Text::new("HDOP:   --   VDOP: --", Point::new(x, y), small)
                .draw(display)
                .ok();
        }
    }
}

pub(crate) fn gps_data_native_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(30, 170, 480, 300)
}

// the map panel below the readout: a static, grayscale map centered on the
// current fix, fetched from noot-server (the device speaks plain http only, so
// it cannot reach a tile server directly; the server renders and downscales).
const MAP_X: i32 = 30;
const MAP_Y: i32 = 500;
const MAP_W: u32 = 480;
const MAP_H: u32 = 400;
// street-level zoom for the requested map.
const MAP_ZOOM: u8 = 15;
// upper bound on the map image body buffered over http. a downscaled grayscale
// jpeg for this panel is well under this; larger responses are dropped.
pub(crate) const MAP_MAX_BYTES: usize = 256 * 1024;

// what the map panel is currently showing.
pub(crate) enum MapView {
    Loading,
    // no GPS position yet, so there is no center to request a map for.
    NoFix,
    Ready(GrayImage),
    Error,
}

// the noot-server request path for a map centered on a position, sized to the
// panel so the server renders it at the right resolution.
pub(crate) fn map_path(lat: f64, lon: f64) -> FmtBuf<96> {
    let mut buf = FmtBuf::<96>::new();
    write!(
        buf,
        "/api/map?lat={lat:.5}&lon={lon:.5}&zoom={MAP_ZOOM}&w={MAP_W}&h={MAP_H}"
    )
    .ok();
    buf
}

// decode a map image response body into a view.
pub(crate) fn parse_map(body: &[u8]) -> MapView {
    match decode_image(body) {
        Ok(img) => MapView::Ready(img),
        Err(_) => MapView::Error,
    }
}

pub(crate) fn draw_map(display: &mut Display, view: &MapView) {
    Rectangle::new(Point::new(MAP_X, MAP_Y), Size::new(MAP_W, MAP_H))
        .into_styled(PrimitiveStyle::with_stroke(Gray4::BLACK, 2))
        .draw(display)
        .ok();

    match view {
        MapView::Loading => map_label(display, "loading map..."),
        MapView::NoFix => map_label(display, "waiting for GPS fix..."),
        MapView::Error => map_label(display, "map unavailable"),
        MapView::Ready(img) => {
            draw_image_fit(display, img, MAP_X, MAP_Y, MAP_W, MAP_H);
            draw_marker(display, MAP_X + MAP_W as i32 / 2, MAP_Y + MAP_H as i32 / 2);
        }
    }
}

// a "you are here" dot at the map center. the white halo and black ring keep it
// visible over any map tone the panel renders underneath.
fn draw_marker(display: &mut Display, cx: i32, cy: i32) {
    let halo = 9;
    Circle::new(Point::new(cx - halo, cy - halo), (halo * 2) as u32)
        .into_styled(PrimitiveStyle::with_fill(Gray4::WHITE))
        .draw(display)
        .ok();
    Circle::new(Point::new(cx - halo, cy - halo), (halo * 2) as u32)
        .into_styled(PrimitiveStyle::with_stroke(Gray4::BLACK, 2))
        .draw(display)
        .ok();
    let dot = 4;
    Circle::new(Point::new(cx - dot, cy - dot), (dot * 2) as u32)
        .into_styled(PrimitiveStyle::with_fill(Gray4::BLACK))
        .draw(display)
        .ok();
}

fn map_label(display: &mut Display, text: &str) {
    let style = MonoTextStyle::new(&FONT_6X10, Gray4::new(4));
    Text::with_alignment(
        text,
        Point::new(MAP_X + MAP_W as i32 / 2, MAP_Y + MAP_H as i32 / 2),
        style,
        Alignment::Center,
    )
    .draw(display)
    .ok();
}
