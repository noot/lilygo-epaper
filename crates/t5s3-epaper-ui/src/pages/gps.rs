use alloc::{string::String, vec::Vec};
use core::{f64::consts::PI, fmt::Write as _};

use embedded_graphics::{
    mono_font::{
        ascii::{FONT_6X10, FONT_9X18_BOLD},
        MonoTextStyle,
    },
    prelude::*,
    primitives::{Circle, PrimitiveStyle, Rectangle},
    text::{Alignment, Text},
};
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use epub_reader::{decode_image, GrayImage};
use t5s3_epaper_core::{display::DrawMode, gps::Gps, Display, SdCard};

use crate::{
    fmt::FmtBuf,
    layout::{screen_to_native_rect, SCREEN_W},
    widgets::{draw_image_fit, draw_image_scaled},
};

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
// how far out from the fix's cell `save area` pre-downloads, in cells. each
// cell is ~1.6 km at MAP_ZOOM, so radius 8 => a 17x17 block (~27 km across,
// ~289 tiles). the ceiling is practical, not technical: the download blocks the
// ui while it runs (~10-15 min for 289 tiles) and each tile makes the server
// pull several OpenStreetMap tiles, so a much larger radius is both a very long
// freeze and impolite to OSM. raise it if you accept those costs; genuinely
// huge areas need a coarser zoom instead (street detail can't cover hundreds of
// km).
pub(crate) const DOWNLOAD_RADIUS: i32 = 8;

// the "save area" button overlaid on the top-right of the map panel.
const DL_BTN_W: u32 = 150;
const DL_BTN_H: u32 = 40;
const DL_BTN_X: i32 = MAP_X + MAP_W as i32 - DL_BTN_W as i32 - 10;
const DL_BTN_Y: i32 = MAP_Y + 10;

// the "full screen" button overlaid on the top-left of the map panel.
const FS_BTN_W: u32 = 150;
const FS_BTN_H: u32 = 40;
const FS_BTN_X: i32 = MAP_X + 10;
const FS_BTN_Y: i32 = MAP_Y + 10;

// what the map panel is currently showing.
pub(crate) enum MapView {
    Loading,
    // no GPS position yet, so there is no center to request a map for.
    NoFix,
    // pre-downloading an area: the number of not-yet-cached tiles being fetched.
    Downloading(usize),
    // finished pre-downloading: the square kilometres now cached around the fix,
    // and how many tiles were newly fetched (0 = the area was already cached).
    Saved { km2: f32, new_tiles: usize },
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

// the cache filename (no directory) for a cell key, for matching a cell against
// a directory listing without a per-tile filesystem lookup.
pub(crate) fn map_cache_filename(key: u32) -> FmtBuf<16> {
    let mut buf = FmtBuf::<16>::new();
    write!(buf, "{key:08X}.JPG").ok();
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
        MapView::Saved { km2, new_tiles } => {
            let mut buf = FmtBuf::<40>::new();
            if *new_tiles == 0 {
                write!(buf, "area already saved: {km2:.0} sq km").ok();
            } else {
                write!(buf, "area saved: {km2:.0} sq km ({new_tiles} new)").ok();
            }
            map_label(display, buf.as_str());
        }
        MapView::Ready { .. } => {}
    }

    draw_download_button(display);
    draw_fullscreen_button(display);
}

// the "full screen" button overlaid on the panel, over a white fill so it stays
// legible on top of the map.
fn draw_fullscreen_button(display: &mut Display) {
    let rect = Rectangle::new(
        Point::new(FS_BTN_X, FS_BTN_Y),
        Size::new(FS_BTN_W, FS_BTN_H),
    );
    rect.into_styled(PrimitiveStyle::with_fill(Gray4::WHITE))
        .draw(display)
        .ok();
    rect.into_styled(PrimitiveStyle::with_stroke(Gray4::BLACK, 2))
        .draw(display)
        .ok();
    let style = MonoTextStyle::new(&FONT_6X10, Gray4::BLACK);
    Text::with_alignment(
        "full screen",
        Point::new(
            FS_BTN_X + FS_BTN_W as i32 / 2,
            FS_BTN_Y + FS_BTN_H as i32 / 2 + 4,
        ),
        style,
        Alignment::Center,
    )
    .draw(display)
    .ok();
}

pub(crate) fn fullscreen_button_hit(sx: i32, sy: i32) -> bool {
    sx >= FS_BTN_X
        && sx < FS_BTN_X + FS_BTN_W as i32
        && sy >= FS_BTN_Y
        && sy < FS_BTN_Y + FS_BTN_H as i32
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

// update the "saving area: done/total" line on a white strip across the panel
// center and fast-refresh just that strip, so a long save-area download shows
// progress instead of a silent frozen screen. called periodically mid-download.
pub(crate) fn show_download_progress(display: &mut Display, done: usize, total: usize) {
    let strip_y = MAP_Y + MAP_H as i32 / 2 - 15;
    Rectangle::new(Point::new(MAP_X, strip_y), Size::new(MAP_W, 30))
        .into_styled(PrimitiveStyle::with_fill(Gray4::WHITE))
        .draw(display)
        .ok();
    let mut buf = FmtBuf::<40>::new();
    write!(buf, "saving area: {done}/{total}...").ok();
    Text::with_alignment(
        buf.as_str(),
        Point::new(MAP_X + MAP_W as i32 / 2, strip_y + 20),
        MonoTextStyle::new(&FONT_6X10, Gray4::BLACK),
        Alignment::Center,
    )
    .draw(display)
    .ok();
    display
        .flush_partial_fast(screen_to_native_rect(MAP_X, strip_y, MAP_W as i32, 30))
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

// fullscreen map view -------------------------------------------------------

// the screen is 960 px tall in the rotated ui coordinate space.
const SCREEN_H: i32 = 960;
// the fullscreen map fills the screen above a bottom control bar. it is
// stitched from cached zoom-15 tiles (no wifi), so it works offline over
// whatever area has been cached or pre-downloaded; uncached tiles are left
// blank.
const FULL_MAP_X: i32 = 0;
const FULL_MAP_Y: i32 = 0;
const FULL_MAP_W: i32 = SCREEN_W;
const FULL_CTRL_H: i32 = 84;
const FULL_MAP_H: i32 = SCREEN_H - FULL_CTRL_H;
// bottom control bar: three equal buttons across its width.
const FULL_BTN_Y: i32 = FULL_MAP_H;
const FULL_BTN_W: i32 = SCREEN_W / 3;
// digital zoom range in powers of two of world-pixels-per-screen-pixel:
// negative magnifies cached tiles (blocky), positive stitches more tiles in
// (more area). bounded so a zoom-out doesn't decode an unreasonable number of
// tiles.
pub(crate) const FULL_ZOOM_MIN: i32 = -1;
pub(crate) const FULL_ZOOM_MAX: i32 = 2;

// which fullscreen control a tap landed on.
pub(crate) enum FullAction {
    ZoomOut,
    ZoomIn,
    Back,
    // a tap on the map itself re-centers on the current fix.
    Recenter,
}

pub(crate) fn full_touch(sx: i32, sy: i32) -> FullAction {
    if sy < FULL_BTN_Y {
        FullAction::Recenter
    } else if sx < FULL_BTN_W {
        FullAction::ZoomOut
    } else if sx < FULL_BTN_W * 2 {
        FullAction::ZoomIn
    } else {
        FullAction::Back
    }
}

// stitch the cached tiles overlapping the viewport (centered on the fix) into
// the map area at the requested digital zoom. the caller clears the screen
// first, so uncached tiles simply stay blank.
pub(crate) fn render_full_map(
    display: &mut Display,
    card: Option<&SdCard>,
    fix: Option<GpsFix>,
    zoom_step: i32,
) {
    let center = Point::new(FULL_MAP_X + FULL_MAP_W / 2, FULL_MAP_Y + FULL_MAP_H / 2);
    let (Some(fix), Some(card)) = (fix, card) else {
        let msg = if fix.is_none() {
            "waiting for GPS fix..."
        } else {
            "no SD card"
        };
        Text::with_alignment(
            msg,
            center,
            MonoTextStyle::new(&FONT_6X10, Gray4::new(4)),
            Alignment::Center,
        )
        .draw(display)
        .ok();
        return;
    };

    // world pixels per screen pixel, as the ratio num/den.
    let (num, den): (f64, f64) = if zoom_step >= 0 {
        ((1i64 << zoom_step) as f64, 1.0)
    } else {
        (1.0, (1i64 << (-zoom_step)) as f64)
    };
    let (fpx, fpy) = project(fix.lat(), fix.lon());
    let view_w = f64::from(FULL_MAP_W) * num / den;
    let view_h = f64::from(FULL_MAP_H) * num / den;
    let vx0 = fpx - view_w / 2.0;
    let vy0 = fpy - view_h / 2.0;

    // world px -> screen px (den/num is screen pixels per world pixel).
    let to_sx = |wx: f64| FULL_MAP_X + libm::round((wx - vx0) * den / num) as i32;
    let to_sy = |wy: f64| FULL_MAP_Y + libm::round((wy - vy0) * den / num) as i32;

    let clip = Rectangle::new(
        Point::new(FULL_MAP_X, FULL_MAP_Y),
        Size::new(FULL_MAP_W as u32, FULL_MAP_H as u32),
    );
    let cx0 = libm::floor(vx0 / f64::from(MAP_W)) as i64;
    let cx1 = libm::floor((vx0 + view_w) / f64::from(MAP_W)) as i64;
    let cy0 = libm::floor(vy0 / f64::from(MAP_H)) as i64;
    let cy1 = libm::floor((vy0 + view_h) / f64::from(MAP_H)) as i64;
    for cy in cy0..=cy1 {
        for cx in cx0..=cx1 {
            let Ok(bytes) = card.read_file(map_cache_path(cell_key(cx, cy)).as_str()) else {
                continue;
            };
            let Ok(img) = decode_image(&bytes) else {
                continue;
            };
            // derive each tile's screen rect from its world corners so
            // neighbours share an edge with no seam.
            let dx0 = to_sx(cx as f64 * f64::from(MAP_W));
            let dx1 = to_sx((cx + 1) as f64 * f64::from(MAP_W));
            let dy0 = to_sy(cy as f64 * f64::from(MAP_H));
            let dy1 = to_sy((cy + 1) as f64 * f64::from(MAP_H));
            draw_image_scaled(
                display,
                &img,
                dx0,
                dy0,
                (dx1 - dx0).max(1) as u32,
                (dy1 - dy0).max(1) as u32,
                clip,
            );
        }
    }

    // the fix sits at the viewport center.
    draw_marker(display, center.x, center.y);
}

// the bottom control bar (zoom out / zoom in / back) plus a zoom-factor readout
// overlaid on the top-left of the map.
pub(crate) fn draw_full_controls(display: &mut Display, zoom_step: i32) {
    Rectangle::new(
        Point::new(0, FULL_BTN_Y),
        Size::new(SCREEN_W as u32, FULL_CTRL_H as u32),
    )
    .into_styled(PrimitiveStyle::with_fill(Gray4::WHITE))
    .draw(display)
    .ok();
    Rectangle::new(Point::new(0, FULL_BTN_Y), Size::new(SCREEN_W as u32, 2))
        .into_styled(PrimitiveStyle::with_fill(Gray4::BLACK))
        .draw(display)
        .ok();
    for i in 1..3 {
        Rectangle::new(
            Point::new(FULL_BTN_W * i, FULL_BTN_Y),
            Size::new(2, FULL_CTRL_H as u32),
        )
        .into_styled(PrimitiveStyle::with_fill(Gray4::BLACK))
        .draw(display)
        .ok();
    }

    let bold = MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK);
    let ty = FULL_BTN_Y + FULL_CTRL_H / 2 + 6;
    for (i, text) in ["zoom -", "zoom +", "back"].iter().enumerate() {
        Text::with_alignment(
            text,
            Point::new(FULL_BTN_W * i as i32 + FULL_BTN_W / 2, ty),
            bold,
            Alignment::Center,
        )
        .draw(display)
        .ok();
    }

    // zoom-factor readout over the top-left of the map.
    let mut buf = FmtBuf::<16>::new();
    match zoom_step {
        0 => {
            write!(buf, "1x").ok();
        }
        s if s > 0 => {
            write!(buf, "out {}x", 1 << s).ok();
        }
        s => {
            write!(buf, "in {}x", 1 << (-s)).ok();
        }
    }
    Rectangle::new(Point::new(FULL_MAP_X, FULL_MAP_Y), Size::new(90, 26))
        .into_styled(PrimitiveStyle::with_fill(Gray4::WHITE))
        .draw(display)
        .ok();
    Text::new(
        buf.as_str(),
        Point::new(FULL_MAP_X + 8, FULL_MAP_Y + 17),
        MonoTextStyle::new(&FONT_6X10, Gray4::BLACK),
    )
    .draw(display)
    .ok();
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
