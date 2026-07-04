use alloc::{string::String, vec::Vec};
use core::{f64::consts::PI, fmt::Write as _};

use embedded_graphics::{
    mono_font::{ascii::FONT_6X10, MonoTextStyle},
    prelude::*,
    primitives::{Circle, PrimitiveStyle, Rectangle},
    text::{Alignment, Text},
};
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use epub_reader::{decode_image, GrayImage};
use t5s3_epaper_core::{display::DrawMode, gps::Gps, Display};

use crate::{fmt::FmtBuf, layout::screen_to_native_rect, widgets::draw_image_fit};

// loop ticks between GPS readout refreshes (~50ms per tick)
pub(crate) const GPS_REFRESH_TICKS: u16 = 30;

// slowest cadence, in loop ticks (~50ms each), at which the map redraws to
// track the marker's movement within a tile. a grayscale panel refresh flashes,
// so this is deliberately slow (~20s) even though the readout updates far more
// often. crossing into a new tile reloads the map immediately regardless.
pub(crate) const MARKER_REFRESH_TICKS: u16 = 400;
// only redraw for within-tile movement once the marker has shifted this many
// panel pixels, so a stationary fix's jitter doesn't keep flashing the panel.
pub(crate) const MARKER_MOVE_PX: i32 = 12;

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

// the map panel below the readout: a grayscale map centered on the current fix,
// fetched from noot-server (the device speaks plain http only, so it cannot
// reach a tile server directly; the server renders and downscales). fetched
// maps are cached on the sd card keyed by their map cell, so the panel works
// offline once a cell has been visited or pre-downloaded.
const MAP_X: i32 = 30;
const MAP_Y: i32 = 500;
const MAP_W: u32 = 480;
const MAP_H: u32 = 400;
// street-level zoom for the requested map.
const MAP_ZOOM: u8 = 15;
// osm raster tiles are 256px square; the web-mercator world is this wide at
// MAP_ZOOM, matching the projection noot-server renders the panel with.
const TILE: f64 = 256.0;
// upper bound on the map image body buffered over http. a downscaled grayscale
// jpeg for this panel is well under this; larger responses are dropped.
pub(crate) const MAP_MAX_BYTES: usize = 256 * 1024;

// where fetched maps are cached. 8.3-safe: a <=8 char dir plus files named from
// a 32-bit cell key (8 hex chars) with a 3-char extension.
pub(crate) const MAP_CACHE_DIR: &str = "/MAPS";
// how far out from the fix's cell `save area` pre-downloads, in cells. radius 2
// => a 5x5 block (~7-8 km across at MAP_ZOOM).
pub(crate) const DOWNLOAD_RADIUS: i32 = 2;

// the "save area" button overlaid on the top-right of the map panel.
const DL_BTN_W: u32 = 150;
const DL_BTN_H: u32 = 40;
const DL_BTN_X: i32 = MAP_X + MAP_W as i32 - DL_BTN_W as i32 - 10;
const DL_BTN_Y: i32 = MAP_Y + 10;

// what the map panel is currently showing.
pub(crate) enum MapView {
    Loading,
    // no GPS position yet, so there is no center to request a map for.
    NoFix,
    // pre-downloading an area: the number of not-yet-cached tiles being fetched.
    Downloading(usize),
    // finished pre-downloading: the square kilometres now cached around the fix.
    Saved(f32),
    // a decoded map plus the marker offset (in panel pixels) from the panel
    // center to the fix, since the map is centered on its cell, not the fix.
    Ready { img: GrayImage, dx: i32, dy: i32 },
    Error,
}

// the sd-cache cell a fix falls in: its cache key (filename) and the marker
// offset from the panel center to that exact fix.
pub(crate) struct MapCell {
    key: u32,
    dx: i32,
    dy: i32,
}

impl MapCell {
    pub(crate) fn key(&self) -> u32 {
        self.key
    }

    pub(crate) fn dx(&self) -> i32 {
        self.dx
    }

    pub(crate) fn dy(&self) -> i32 {
        self.dy
    }
}

// project (lat, lon) to web-mercator world pixels at MAP_ZOOM. matches
// noot-server's projection so a cell center on the device maps to the same crop
// the server would render.
fn project(lat: f64, lon: f64) -> (f64, f64) {
    let world = TILE * f64::from(1u32 << MAP_ZOOM);
    let lat = lat.clamp(-85.051_128_78, 85.051_128_78);
    let lat_rad = lat * PI / 180.0;
    let px = (lon + 180.0) / 360.0 * world;
    let py = (1.0 - libm::log(libm::tan(lat_rad) + 1.0 / libm::cos(lat_rad)) / PI) / 2.0 * world;
    (px, py)
}

// inverse of `project`: web-mercator world pixels back to (lat, lon).
fn unproject(px: f64, py: f64) -> (f64, f64) {
    let world = TILE * f64::from(1u32 << MAP_ZOOM);
    let lon = px / world * 360.0 - 180.0;
    let lat_rad = libm::atan(libm::sinh(PI * (1.0 - 2.0 * py / world)));
    (lat_rad * 180.0 / PI, lon)
}

// the integer cell (one MAP_W x MAP_H panel) a fix falls in.
fn cell_indices(lat: f64, lon: f64) -> (i64, i64) {
    let (px, py) = project(lat, lon);
    (
        libm::floor(px / f64::from(MAP_W)) as i64,
        libm::floor(py / f64::from(MAP_H)) as i64,
    )
}

// pack a cell's indices into its cache key. both indices fit in 16 bits at
// MAP_ZOOM (the world is <2^16 panels wide/tall).
fn cell_key(cx: i64, cy: i64) -> u32 {
    ((cx as u32 & 0xFFFF) << 16) | (cy as u32 & 0xFFFF)
}

// the (lat, lon) at the center of a cell, used as the map request center so any
// fix inside the cell reuses the same cached image.
fn cell_center(key: u32) -> (f64, f64) {
    let cx = i64::from(key >> 16);
    let cy = i64::from(key & 0xFFFF);
    unproject(
        (cx as f64 + 0.5) * f64::from(MAP_W),
        (cy as f64 + 0.5) * f64::from(MAP_H),
    )
}

// locate the cell a fix falls in and the marker offset for that exact fix.
pub(crate) fn map_cell(lat: f64, lon: f64) -> MapCell {
    let (px, py) = project(lat, lon);
    let cx = libm::floor(px / f64::from(MAP_W)) as i64;
    let cy = libm::floor(py / f64::from(MAP_H)) as i64;
    let center_px = (cx as f64 + 0.5) * f64::from(MAP_W);
    let center_py = (cy as f64 + 0.5) * f64::from(MAP_H);
    MapCell {
        key: cell_key(cx, cy),
        dx: libm::round(px - center_px) as i32,
        dy: libm::round(py - center_py) as i32,
    }
}

// the sd cache path for a cell key.
pub(crate) fn map_cache_path(key: u32) -> FmtBuf<24> {
    let mut buf = FmtBuf::<24>::new();
    write!(buf, "{MAP_CACHE_DIR}/{key:08X}.JPG").ok();
    buf
}

// the noot-server request path for a cell's map, centered on the cell and sized
// to the panel so the server renders it at the right resolution.
pub(crate) fn map_request_path(key: u32) -> FmtBuf<96> {
    let (lat, lon) = cell_center(key);
    let mut buf = FmtBuf::<96>::new();
    write!(
        buf,
        "/api/map?lat={lat:.5}&lon={lon:.5}&zoom={MAP_ZOOM}&w={MAP_W}&h={MAP_H}"
    )
    .ok();
    buf
}

// the ground area, in square kilometres, that a `radius`-cell block covers at
// the fix's latitude. web-mercator is conformal, so a panel pixel spans the
// same distance in x and y locally.
pub(crate) fn area_km2(lat: f64, radius: i32) -> f32 {
    let lat_rad = lat.clamp(-85.051_128_78, 85.051_128_78) * PI / 180.0;
    let meters_per_px = 156_543.033_928 * libm::cos(lat_rad) / f64::from(1u32 << MAP_ZOOM);
    let cells = f64::from(2 * radius + 1);
    let w_km = cells * f64::from(MAP_W) * meters_per_px / 1000.0;
    let h_km = cells * f64::from(MAP_H) * meters_per_px / 1000.0;
    (w_km * h_km) as f32
}

// the (key, request-path) of every cell in a `radius`-cell block around a fix,
// for pre-downloading an area. the caller filters out already-cached cells.
pub(crate) fn map_area_tiles(lat: f64, lon: f64, radius: i32) -> Vec<(u32, String)> {
    let (cx, cy) = cell_indices(lat, lon);
    let mut tiles = Vec::new();
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            let key = cell_key(cx + i64::from(dx), cy + i64::from(dy));
            tiles.push((key, String::from(map_request_path(key).as_str())));
        }
    }
    tiles
}

// decode a map image response body into a view, tagging it with the marker
// offset for the fix that requested it.
pub(crate) fn parse_map(body: &[u8], dx: i32, dy: i32) -> MapView {
    match decode_image(body) {
        Ok(img) => MapView::Ready { img, dx, dy },
        Err(_) => MapView::Error,
    }
}

pub(crate) fn draw_map(display: &mut Display, view: &MapView) {
    match view {
        MapView::Ready { img, dx, dy } => {
            draw_image_fit(display, img, MAP_X, MAP_Y, MAP_W, MAP_H);
            draw_marker(
                display,
                MAP_X + MAP_W as i32 / 2 + dx,
                MAP_Y + MAP_H as i32 / 2 + dy,
            );
        }
        // clear the panel so a stale map isn't left under the status label.
        _ => {
            Rectangle::new(Point::new(MAP_X, MAP_Y), Size::new(MAP_W, MAP_H))
                .into_styled(PrimitiveStyle::with_fill(Gray4::WHITE))
                .draw(display)
                .ok();
        }
    }

    Rectangle::new(Point::new(MAP_X, MAP_Y), Size::new(MAP_W, MAP_H))
        .into_styled(PrimitiveStyle::with_stroke(Gray4::BLACK, 2))
        .draw(display)
        .ok();

    match view {
        MapView::Loading => map_label(display, "loading map..."),
        MapView::NoFix => map_label(display, "waiting for GPS fix..."),
        MapView::Error => map_label(display, "map unavailable (offline?)"),
        MapView::Downloading(n) => {
            let mut buf = FmtBuf::<32>::new();
            write!(buf, "saving area: {n} tiles...").ok();
            map_label(display, buf.as_str());
        }
        MapView::Saved(km2) => {
            let mut buf = FmtBuf::<32>::new();
            write!(buf, "area saved: {km2:.0} sq km").ok();
            map_label(display, buf.as_str());
        }
        MapView::Ready { .. } => {}
    }

    draw_download_button(display);
}

// clear the panel then redraw and flush it in grayscale. the physical clear
// gives the grayscale waveform a clean white baseline (as a full-page redraw
// gets from `clear`), so a previous label or marker doesn't ghost through, and
// the flush is scoped to the panel so the rest of the page doesn't flash.
pub(crate) fn refresh_map_panel(display: &mut Display, view: &MapView) {
    display.clear_area(map_panel_native_rect()).ok();
    draw_map(display, view);
    display
        .flush_partial(map_panel_native_rect(), DrawMode::BlackOnWhite)
        .ok();
}

// the "save area" button overlaid on the panel, over a white fill so it stays
// legible on top of the map.
fn draw_download_button(display: &mut Display) {
    let rect = Rectangle::new(
        Point::new(DL_BTN_X, DL_BTN_Y),
        Size::new(DL_BTN_W, DL_BTN_H),
    );
    rect.into_styled(PrimitiveStyle::with_fill(Gray4::WHITE))
        .draw(display)
        .ok();
    rect.into_styled(PrimitiveStyle::with_stroke(Gray4::BLACK, 2))
        .draw(display)
        .ok();
    let style = MonoTextStyle::new(&FONT_6X10, Gray4::BLACK);
    Text::with_alignment(
        "save area",
        Point::new(
            DL_BTN_X + DL_BTN_W as i32 / 2,
            DL_BTN_Y + DL_BTN_H as i32 / 2 + 4,
        ),
        style,
        Alignment::Center,
    )
    .draw(display)
    .ok();
}

pub(crate) fn download_button_hit(sx: i32, sy: i32) -> bool {
    sx >= DL_BTN_X
        && sx < DL_BTN_X + DL_BTN_W as i32
        && sy >= DL_BTN_Y
        && sy < DL_BTN_Y + DL_BTN_H as i32
}

// the map panel's rectangle in native (pre-rotation) coordinates, for partial
// refreshes of just the panel.
fn map_panel_native_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(MAP_X, MAP_Y, MAP_W as i32, MAP_H as i32)
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
