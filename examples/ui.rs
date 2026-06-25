#![no_std]
#![no_main]

extern crate alloc;
extern crate lilygo_t5s3paperpro;

use alloc::vec::Vec;
use core::fmt::Write as _;

use embedded_graphics::{
    image::Image,
    mono_font::{
        ascii::{FONT_6X10, FONT_9X15, FONT_9X18_BOLD},
        MonoTextStyle,
    },
    prelude::*,
    primitives::{Circle, PrimitiveStyle, PrimitiveStyleBuilder, Rectangle, RoundedRectangle},
    text::{Alignment, Text},
};
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use esp_backtrace as _;
use esp_hal::{delay::Delay, main, rng::Rng};
use lilygo_t5s3paperpro::{
    display::DisplayRotation,
    pin_config,
    sdcard_pin_config,
    Display,
    DrawMode,
    FrontLight,
    SdCard,
};
#[cfg(feature = "gps")]
use lilygo_t5s3paperpro::{gps::Gps, gps_pin_config};
use tinybmp::Bmp;

esp_bootloader_esp_idf::esp_app_desc!();

// ── layout constants ────────────────────────────────────────────────
const SCREEN_W: i32 = 540;
const STATUS_H: i32 = 55;

const COLS: usize = 2;
const ICON_W: u16 = 220;
const ICON_H: u16 = 240;
const GAP_X: u16 = 20;
const GAP_Y: u16 = 30;
const GRID_TOP_Y: i32 = STATUS_H + 40;

const BRIGHTNESS_STEP: u8 = 3;
const FL_BAR_X: i32 = 70;
const FL_BAR_Y: i32 = 380;
const FL_BAR_W: u32 = 400;
const FL_BAR_H: u32 = 50;
const FL_BTN_Y: i32 = 500;
const FL_BTN_W: u32 = 150;
const FL_BTN_H: u32 = 70;
const FL_MINUS_X: i32 = 80;
const FL_PLUS_X: i32 = 310;

const SLEEP_BTN_X: i32 = 160;
const SLEEP_BTN_Y: i32 = 420;
const SLEEP_BTN_W: u32 = 220;
const SLEEP_BTN_H: u32 = 90;

// folder of 540x960 grayscale .bmp wallpapers on the SD card; one is picked at
// random as the sleep screensaver. this must be a FAT 8.3 name (<=8 chars), as
// must the .bmp files inside it, so they can be opened by name.
const WALLPAPER_DIR: &str = "/WALLS";

// loop ticks between GPS readout refreshes (~50ms per tick)
#[cfg(feature = "gps")]
const GPS_REFRESH_TICKS: u16 = 30;

// ── formatting helper ───────────────────────────────────────────────
struct FmtBuf<const N: usize> {
    buf: [u8; N],
    pos: usize,
}

impl<const N: usize> FmtBuf<N> {
    fn new() -> Self {
        Self {
            buf: [0; N],
            pos: 0,
        }
    }

    #[cfg_attr(not(feature = "gps"), allow(dead_code))]
    fn reset(&mut self) {
        self.pos = 0;
    }

    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.pos]).unwrap_or("")
    }
}

impl<const N: usize> core::fmt::Write for FmtBuf<N> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let bytes = s.as_bytes();
        let len = bytes.len().min(N - self.pos);
        self.buf[self.pos..self.pos + len].copy_from_slice(&bytes[..len]);
        self.pos += len;
        Ok(())
    }
}

// ── coordinate helpers ──────────────────────────────────────────────
// Rotate270: screen(x,y) → native(y, 539-x)
// Inverse for touch: screen_x = 539 - native_y, screen_y = native_x
fn touch_to_screen(tx: u16, ty: u16) -> (i32, i32) {
    (539 - ty as i32, tx as i32)
}

fn screen_to_native_rect(
    sx: i32,
    sy: i32,
    sw: i32,
    sh: i32,
) -> lilygo_t5s3paperpro::display::Rectangle {
    lilygo_t5s3paperpro::display::Rectangle {
        x: sy as u16,
        y: (Display::HEIGHT as i32 - sx - sw) as u16,
        width: sh as u16,
        height: sw as u16,
    }
}

// ── screens ─────────────────────────────────────────────────────────
#[derive(Clone, Copy, PartialEq)]
enum Screen {
    Home,
    Gps,
    Lora,
    Frontlight,
    Sleep,
}

impl Screen {
    fn to_index(self) -> u8 {
        match self {
            Screen::Home => 0,
            Screen::Gps => 1,
            Screen::Lora => 2,
            Screen::Frontlight => 3,
            Screen::Sleep => 4,
        }
    }

    // map a stored index back to a screen. the Sleep screen and any
    // unexpected value fall back to Home, so waking never lands on the sleep
    // menu or on garbage left by an interrupted persistent write.
    fn from_index(value: u8) -> Self {
        match value {
            1 => Screen::Gps,
            2 => Screen::Lora,
            3 => Screen::Frontlight,
            _ => Screen::Home,
        }
    }
}

// last visited screen, stored in RTC fast memory so it survives the reset that
// deep sleep performs. zeroed (Home) on first boot, then retained across sleep.
#[esp_hal::ram(unstable(rtc_fast, persistent))]
static mut LAST_SCREEN: u8 = 0;

struct Icon {
    label: &'static str,
    glyph: &'static str,
    screen: Screen,
}

const ICONS: [Icon; 4] = [
    Icon {
        label: "GPS",
        glyph: "GPS",
        screen: Screen::Gps,
    },
    Icon {
        label: "LoRa",
        glyph: "RF",
        screen: Screen::Lora,
    },
    Icon {
        label: "Light",
        glyph: "LIT",
        screen: Screen::Frontlight,
    },
    Icon {
        label: "Sleep",
        glyph: "ZZZ",
        screen: Screen::Sleep,
    },
];

fn grid_origin_x() -> i32 {
    (SCREEN_W - (COLS as i32 * ICON_W as i32 + (COLS as i32 - 1) * GAP_X as i32)) / 2
}

fn icon_rect(idx: usize) -> (i32, i32) {
    let col = idx % COLS;
    let row = idx / COLS;
    (
        grid_origin_x() + (col as i32) * (ICON_W as i32 + GAP_X as i32),
        GRID_TOP_Y + (row as i32) * (ICON_H as i32 + GAP_Y as i32),
    )
}

fn hit_test(sx: i32, sy: i32) -> Option<usize> {
    for i in 0..ICONS.len() {
        let (x, y) = icon_rect(i);
        if sx >= x && sx < x + ICON_W as i32 && sy >= y && sy < y + ICON_H as i32 {
            return Some(i);
        }
    }
    None
}

fn back_button_hit(sx: i32, sy: i32) -> bool {
    sx < 110 && sy < 50
}

fn minus_hit(sx: i32, sy: i32) -> bool {
    (FL_MINUS_X..FL_MINUS_X + FL_BTN_W as i32).contains(&sx)
        && (FL_BTN_Y..FL_BTN_Y + FL_BTN_H as i32).contains(&sy)
}

fn plus_hit(sx: i32, sy: i32) -> bool {
    (FL_PLUS_X..FL_PLUS_X + FL_BTN_W as i32).contains(&sx)
        && (FL_BTN_Y..FL_BTN_Y + FL_BTN_H as i32).contains(&sy)
}

fn sleep_now_hit(sx: i32, sy: i32) -> bool {
    (SLEEP_BTN_X..SLEEP_BTN_X + SLEEP_BTN_W as i32).contains(&sx)
        && (SLEEP_BTN_Y..SLEEP_BTN_Y + SLEEP_BTN_H as i32).contains(&sy)
}

// ── drawing: status bar ─────────────────────────────────────────────
fn draw_battery_icon(display: &mut Display, x: i32, y: i32, pct: u16) {
    let body_w: u32 = 30;
    let body_h: u32 = 20;
    let nub_w: u32 = 3;
    let nub_h: u32 = 10;

    // body outline
    Rectangle::new(Point::new(x, y), Size::new(body_w, body_h))
        .into_styled(
            PrimitiveStyleBuilder::new()
                .stroke_color(Gray4::BLACK)
                .stroke_width(2)
                .build(),
        )
        .draw(display)
        .ok();

    // nub
    Rectangle::new(
        Point::new(x + body_w as i32, y + (body_h as i32 - nub_h as i32) / 2),
        Size::new(nub_w, nub_h),
    )
    .into_styled(PrimitiveStyle::with_fill(Gray4::BLACK))
    .draw(display)
    .ok();

    // fill
    let inner_w = body_w - 6;
    let fill_w = inner_w * pct.min(100) as u32 / 100;
    if fill_w > 0 {
        Rectangle::new(Point::new(x + 3, y + 3), Size::new(fill_w, body_h - 6))
            .into_styled(PrimitiveStyle::with_fill(Gray4::new(4)))
            .draw(display)
            .ok();
    }
}

fn draw_status_bar(display: &mut Display, voltage: f32, pct: u16, temp: i8) {
    let status_font = MonoTextStyle::new(&FONT_9X15, Gray4::BLACK);

    let v_int = voltage as u32;
    let v_frac = ((voltage - v_int as f32) * 100.0) as u32;
    let mut buf = FmtBuf::<48>::new();
    write!(
        buf,
        "{}%  {}.{:02}V  {}C",
        pct.min(100),
        v_int,
        v_frac,
        temp
    )
    .ok();
    Text::with_alignment(
        buf.as_str(),
        Point::new(491, 35),
        status_font,
        Alignment::Right,
    )
    .draw(display)
    .ok();

    draw_battery_icon(display, 497, 20, pct);

    Rectangle::new(Point::new(0, STATUS_H - 2), Size::new(SCREEN_W as u32, 2))
        .into_styled(PrimitiveStyle::with_fill(Gray4::new(8)))
        .draw(display)
        .ok();
}

// ── drawing: home ───────────────────────────────────────────────────
fn draw_home(display: &mut Display) {
    let bold = MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK);
    let small = MonoTextStyle::new(&FONT_6X10, Gray4::new(4));

    Text::with_alignment("T5 S3 Pro", Point::new(15, 37), bold, Alignment::Left)
        .draw(display)
        .ok();
    let border = PrimitiveStyleBuilder::new()
        .stroke_color(Gray4::BLACK)
        .stroke_width(2)
        .fill_color(Gray4::WHITE)
        .build();

    for (i, icon) in ICONS.iter().enumerate() {
        let (x, y) = icon_rect(i);

        RoundedRectangle::with_equal_corners(
            Rectangle::new(Point::new(x, y), Size::new(ICON_W as u32, ICON_H as u32)),
            Size::new(16, 16),
        )
        .into_styled(border)
        .draw(display)
        .ok();

        Text::with_alignment(
            icon.glyph,
            Point::new(x + ICON_W as i32 / 2, y + ICON_H as i32 / 2 - 15),
            bold,
            Alignment::Center,
        )
        .draw(display)
        .ok();

        Text::with_alignment(
            icon.label,
            Point::new(x + ICON_W as i32 / 2, y + ICON_H as i32 / 2 + 20),
            small,
            Alignment::Center,
        )
        .draw(display)
        .ok();
    }
}

// ── drawing: back button ────────────────────────────────────────────
fn draw_back_button(display: &mut Display) {
    let bold = MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK);
    let border = PrimitiveStyleBuilder::new()
        .stroke_color(Gray4::BLACK)
        .stroke_width(2)
        .fill_color(Gray4::WHITE)
        .build();

    RoundedRectangle::with_equal_corners(
        Rectangle::new(Point::new(10, 10), Size::new(100, 40)),
        Size::new(8, 8),
    )
    .into_styled(border)
    .draw(display)
    .ok();

    Text::with_alignment("< Back", Point::new(60, 36), bold, Alignment::Center)
        .draw(display)
        .ok();
}

// ── drawing: frontlight ─────────────────────────────────────────────
fn draw_frontlight_screen(display: &mut Display, brightness: u8) {
    let bold = MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK);
    draw_back_button(display);

    Text::with_alignment(
        "Frontlight",
        Point::new(SCREEN_W / 2, 120),
        bold,
        Alignment::Center,
    )
    .draw(display)
    .ok();

    draw_brightness_area(display, brightness);

    let btn_border = PrimitiveStyleBuilder::new()
        .stroke_color(Gray4::BLACK)
        .stroke_width(3)
        .fill_color(Gray4::WHITE)
        .build();

    RoundedRectangle::with_equal_corners(
        Rectangle::new(
            Point::new(FL_MINUS_X, FL_BTN_Y),
            Size::new(FL_BTN_W, FL_BTN_H),
        ),
        Size::new(12, 12),
    )
    .into_styled(btn_border)
    .draw(display)
    .ok();

    Text::with_alignment(
        "-",
        Point::new(
            FL_MINUS_X + FL_BTN_W as i32 / 2,
            FL_BTN_Y + FL_BTN_H as i32 / 2 + 6,
        ),
        bold,
        Alignment::Center,
    )
    .draw(display)
    .ok();

    RoundedRectangle::with_equal_corners(
        Rectangle::new(
            Point::new(FL_PLUS_X, FL_BTN_Y),
            Size::new(FL_BTN_W, FL_BTN_H),
        ),
        Size::new(12, 12),
    )
    .into_styled(btn_border)
    .draw(display)
    .ok();

    Text::with_alignment(
        "+",
        Point::new(
            FL_PLUS_X + FL_BTN_W as i32 / 2,
            FL_BTN_Y + FL_BTN_H as i32 / 2 + 6,
        ),
        bold,
        Alignment::Center,
    )
    .draw(display)
    .ok();
}

fn draw_brightness_area(display: &mut Display, brightness: u8) {
    let bold = MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK);

    let mut buf = FmtBuf::<8>::new();
    write!(buf, "{}%", brightness).ok();
    Text::with_alignment(
        buf.as_str(),
        Point::new(SCREEN_W / 2, 330),
        bold,
        Alignment::Center,
    )
    .draw(display)
    .ok();

    Rectangle::new(
        Point::new(FL_BAR_X, FL_BAR_Y),
        Size::new(FL_BAR_W, FL_BAR_H),
    )
    .into_styled(
        PrimitiveStyleBuilder::new()
            .stroke_color(Gray4::BLACK)
            .stroke_width(2)
            .build(),
    )
    .draw(display)
    .ok();

    let fill_w = FL_BAR_W * brightness.min(100) as u32 / 100;
    if fill_w > 4 {
        Rectangle::new(
            Point::new(FL_BAR_X + 3, FL_BAR_Y + 3),
            Size::new(fill_w - 4, FL_BAR_H - 6),
        )
        .into_styled(PrimitiveStyle::with_fill(Gray4::new(6)))
        .draw(display)
        .ok();
    }
}

fn brightness_native_rect() -> lilygo_t5s3paperpro::display::Rectangle {
    screen_to_native_rect(40, 290, 460, 160)
}

// ── drawing: GPS ────────────────────────────────────────────────────
#[cfg(feature = "gps")]
fn draw_gps_data(display: &mut Display, gps: &Gps<'_>) {
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
        lilygo_t5s3paperpro::gps::Module::L76K => "L76K",
        lilygo_t5s3paperpro::gps::Module::MiaM10Q => "MIA-M10Q",
    };

    let fix_str = match gps.fix_type() {
        Some(nmea::sentences::FixType::Gps) => "GPS",
        Some(nmea::sentences::FixType::DGps) => "DGPS",
        Some(nmea::sentences::FixType::Rtk) => "RTK",
        Some(nmea::sentences::FixType::FloatRtk) => "float RTK",
        Some(_) => "other",
        None => "no fix",
    };

    let sats = gps.fix_satellites().unwrap_or(0);
    let mut buf = FmtBuf::<48>::new();

    write!(buf, "Module: {}", module_name).ok();
    Text::new(buf.as_str(), Point::new(x, y), small)
        .draw(display)
        .ok();
    y += line_h;

    buf.reset();
    write!(buf, "Fix:    {} ({} sats)", fix_str, sats).ok();
    Text::new(buf.as_str(), Point::new(x, y), small)
        .draw(display)
        .ok();
    y += line_h + 10;

    if let Some((lat, lng)) = gps.location() {
        buf.reset();
        write!(buf, "Lat:    {:.6}", lat).ok();
        Text::new(buf.as_str(), Point::new(x, y), small)
            .draw(display)
            .ok();
        y += line_h;

        buf.reset();
        write!(buf, "Lon:    {:.6}", lng).ok();
        Text::new(buf.as_str(), Point::new(x, y), small)
            .draw(display)
            .ok();
        y += line_h;
    } else {
        Text::new("Lat:    --", Point::new(x, y), small)
            .draw(display)
            .ok();
        y += line_h;
        Text::new("Lon:    --", Point::new(x, y), small)
            .draw(display)
            .ok();
        y += line_h;
    }

    let alt = gps.altitude().unwrap_or(0.0);
    buf.reset();
    write!(buf, "Alt:    {:.1} m", alt).ok();
    Text::new(buf.as_str(), Point::new(x, y), small)
        .draw(display)
        .ok();
    y += line_h;

    let speed = gps.speed_over_ground().unwrap_or(0.0);
    buf.reset();
    write!(buf, "Speed:  {:.1} kn", speed).ok();
    Text::new(buf.as_str(), Point::new(x, y), small)
        .draw(display)
        .ok();
    y += line_h;

    let hdop = gps.hdop().unwrap_or(0.0);
    let vdop = gps.vdop().unwrap_or(0.0);
    buf.reset();
    write!(buf, "HDOP:   {:.1}   VDOP: {:.1}", hdop, vdop).ok();
    Text::new(buf.as_str(), Point::new(x, y), small)
        .draw(display)
        .ok();
}

#[cfg(feature = "gps")]
fn gps_data_native_rect() -> lilygo_t5s3paperpro::display::Rectangle {
    screen_to_native_rect(30, 170, 480, 300)
}

// ── drawing: LoRa ───────────────────────────────────────────────────
fn draw_lora_screen(display: &mut Display) {
    let bold = MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK);
    let small = MonoTextStyle::new(&FONT_6X10, Gray4::new(4));
    draw_back_button(display);

    Text::with_alignment(
        "LoRa",
        Point::new(SCREEN_W / 2, 120),
        bold,
        Alignment::Center,
    )
    .draw(display)
    .ok();
    Text::with_alignment(
        "SX1262 915MHz",
        Point::new(SCREEN_W / 2, 400),
        small,
        Alignment::Center,
    )
    .draw(display)
    .ok();
}

// ── drawing: sleep ──────────────────────────────────────────────────
fn draw_sleep_screen(display: &mut Display) {
    let bold = MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK);
    let small = MonoTextStyle::new(&FONT_9X15, Gray4::BLACK);
    draw_back_button(display);

    Text::with_alignment(
        "Deep Sleep",
        Point::new(SCREEN_W / 2, 120),
        bold,
        Alignment::Center,
    )
    .draw(display)
    .ok();
    Text::with_alignment(
        "Sleep to save power.",
        Point::new(SCREEN_W / 2, 300),
        small,
        Alignment::Center,
    )
    .draw(display)
    .ok();
    Text::with_alignment(
        "Wake with the BOOT button.",
        Point::new(SCREEN_W / 2, 330),
        small,
        Alignment::Center,
    )
    .draw(display)
    .ok();

    let btn_border = PrimitiveStyleBuilder::new()
        .stroke_color(Gray4::BLACK)
        .stroke_width(3)
        .fill_color(Gray4::WHITE)
        .build();
    RoundedRectangle::with_equal_corners(
        Rectangle::new(
            Point::new(SLEEP_BTN_X, SLEEP_BTN_Y),
            Size::new(SLEEP_BTN_W, SLEEP_BTN_H),
        ),
        Size::new(12, 12),
    )
    .into_styled(btn_border)
    .draw(display)
    .ok();
    Text::with_alignment(
        "Sleep Now",
        Point::new(
            SLEEP_BTN_X + SLEEP_BTN_W as i32 / 2,
            SLEEP_BTN_Y + SLEEP_BTN_H as i32 / 2 + 6,
        ),
        bold,
        Alignment::Center,
    )
    .draw(display)
    .ok();
}

// ── drawing: screensaver (shown while in deep sleep) ─────────────────
fn draw_screensaver(display: &mut Display, pct: u16) {
    let bold = MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK);
    let small = MonoTextStyle::new(&FONT_9X15, Gray4::BLACK);

    let cx = SCREEN_W / 2;
    let cy = 350;
    let r = 90;

    // crescent moon: a black disc with an offset white disc carving it
    Circle::new(Point::new(cx - r, cy - r), (r * 2) as u32)
        .into_styled(PrimitiveStyle::with_fill(Gray4::BLACK))
        .draw(display)
        .ok();
    Circle::new(Point::new(cx - r + 40, cy - r - 30), (r * 2) as u32)
        .into_styled(PrimitiveStyle::with_fill(Gray4::WHITE))
        .draw(display)
        .ok();

    // a scattering of stars
    for (star_x, star_y, star_r) in [
        (150, 230, 5u32),
        (415, 300, 7),
        (385, 470, 4),
        (165, 500, 5),
    ] {
        Circle::new(
            Point::new(star_x - star_r as i32, star_y - star_r as i32),
            star_r * 2,
        )
        .into_styled(PrimitiveStyle::with_fill(Gray4::BLACK))
        .draw(display)
        .ok();
    }

    Text::with_alignment("Sleeping", Point::new(cx, 580), bold, Alignment::Center)
        .draw(display)
        .ok();
    Text::with_alignment(
        "Press the BOOT button to wake",
        Point::new(cx, 630),
        small,
        Alignment::Center,
    )
    .draw(display)
    .ok();

    let mut buf = FmtBuf::<24>::new();
    write!(buf, "Battery {}%", pct.min(100)).ok();
    Text::with_alignment(buf.as_str(), Point::new(cx, 700), small, Alignment::Center)
        .draw(display)
        .ok();
}

// load the wallpaper bitmap from the SD card and draw it full-screen. returns
// false if the card, file, or bitmap is missing or unreadable so the caller can
// fall back to the drawn screensaver.
fn show_wallpaper<'d>(
    display: &mut Display,
    spi: esp_hal::peripherals::SPI2<'d>,
    pins: lilygo_t5s3paperpro::sdcard::PinConfig<'d>,
) -> bool {
    let Ok(sdcard) = SdCard::new(pins, spi) else {
        return false;
    };
    let Ok(entries) = sdcard.list_dir(WALLPAPER_DIR) else {
        return false;
    };

    let mut paths = Vec::new();
    for entry in entries {
        if !entry.is_directory && is_bmp(&entry.name) {
            paths.push(entry.path);
        }
    }
    if paths.is_empty() {
        return false;
    }

    // pick one at random, then fall through the rest so an unreadable file
    // (e.g. a long filename the FAT layer can't open by its 8.3 name) is
    // skipped rather than aborting.
    let start = Rng::new().random() as usize % paths.len();
    for offset in 0..paths.len() {
        let path = &paths[(start + offset) % paths.len()];
        let Ok(bytes) = sdcard.read_file(path) else {
            continue;
        };
        let Ok(bmp) = Bmp::<Gray4>::from_slice(&bytes) else {
            continue;
        };
        if Image::new(&bmp, Point::zero()).draw(display).is_ok() {
            return true;
        }
    }
    false
}

fn is_bmp(name: &str) -> bool {
    name.rsplit_once('.')
        .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("bmp"))
}

// ── main ────────────────────────────────────────────────────────────
#[main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();

    let config = esp_hal::Config::default().with_cpu_clock(esp_hal::clock::CpuClock::_240MHz);
    let peripherals = esp_hal::init(config);
    esp_alloc::psram_allocator!(peripherals.PSRAM, esp_hal::psram);

    let mut display = Display::new(
        pin_config!(peripherals),
        peripherals.I2C0,
        peripherals.DMA_CH0,
        peripherals.LCD_CAM,
        peripherals.RMT,
    )
    .expect("to initialize display");

    display.set_rotation(DisplayRotation::Rotate270);

    let mut light =
        FrontLight::new(peripherals.LEDC, peripherals.GPIO11).expect("to initialize front light");

    let delay = Delay::new();
    display.power_on().expect("to power on");
    delay.delay_millis(10);
    display.clear().expect("to clear");

    // detect the GPS module after the display has been powered on and
    // cleared, matching the working gps example. the GPS power rail is
    // enabled back in Display::new(), and powering + clearing the panel
    // takes long enough for the module to finish booting, so the L76K
    // reliably accepts the sentence-rate configuration sent during
    // detection. detecting right after Display::new() instead leaves the
    // module emitting its full default sentence set, which overruns the
    // 128-byte UART FIFO and prevents a fix from ever assembling.
    #[cfg(feature = "gps")]
    let mut gps: Option<Gps<'_>> = {
        let mut detect_delay = Delay::new();
        match Gps::detect(
            peripherals.UART1,
            gps_pin_config!(peripherals),
            &mut detect_delay,
        ) {
            Ok(g) => {
                esp_println::println!("detected GPS module: {:?}", g.module());
                Some(g)
            }
            Err(e) => {
                esp_println::println!("gps detection failed: {}", e);
                None
            }
        }
    };

    #[cfg(feature = "gps")]
    if let Some(ref mut g) = gps {
        for _ in 0..50 {
            g.update().ok();
            delay.delay_millis(20);
        }
    }

    // restore the screen we slept on. only trust the stored value when we
    // actually woke from deep sleep; any other reset starts at Home. reading
    // the RTC-backed static is sound here as the UI is single-threaded.
    let mut current_screen = if lilygo_t5s3paperpro::power::wake_status().woke_from_deep_sleep() {
        Screen::from_index(unsafe { LAST_SCREEN })
    } else {
        Screen::Home
    };
    let mut needs_redraw = true;
    let mut brightness: u8 = 0;

    #[cfg(feature = "gps")]
    let mut gps_refresh: u16 = 0;

    loop {
        if needs_redraw {
            let voltage = display.battery_voltage().unwrap_or(0.0);
            let pct = display.battery_percentage().unwrap_or(0);
            let temp = display.panel_temperature().unwrap_or(0);

            display.clear().ok();
            draw_status_bar(&mut display, voltage, pct, temp);
            match current_screen {
                Screen::Home => draw_home(&mut display),
                Screen::Gps => {
                    let bold = MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK);
                    draw_back_button(&mut display);
                    Text::with_alignment(
                        "GPS",
                        Point::new(SCREEN_W / 2, 120),
                        bold,
                        Alignment::Center,
                    )
                    .draw(&mut display)
                    .ok();

                    #[cfg(feature = "gps")]
                    match &gps {
                        Some(g) => draw_gps_data(&mut display, g),
                        None => {
                            let small = MonoTextStyle::new(&FONT_6X10, Gray4::new(4));
                            Text::with_alignment(
                                "no module detected",
                                Point::new(SCREEN_W / 2, 400),
                                small,
                                Alignment::Center,
                            )
                            .draw(&mut display)
                            .ok();
                        }
                    }

                    #[cfg(not(feature = "gps"))]
                    {
                        let small = MonoTextStyle::new(&FONT_6X10, Gray4::new(4));
                        Text::with_alignment(
                            "compile with --features gps",
                            Point::new(SCREEN_W / 2, 400),
                            small,
                            Alignment::Center,
                        )
                        .draw(&mut display)
                        .ok();
                    }
                }
                Screen::Lora => draw_lora_screen(&mut display),
                Screen::Frontlight => draw_frontlight_screen(&mut display, brightness),
                Screen::Sleep => draw_sleep_screen(&mut display),
            }
            display.flush(DrawMode::BlackOnWhite).expect("to flush");
            needs_redraw = false;

            #[cfg(feature = "gps")]
            {
                gps_refresh = 0;
            }
        }

        // poll touch/buttons every pass so input stays responsive. the GPS
        // work below is non-blocking, so it never stalls this poll.
        let input = display.input().expect("to read input");

        if input.buttons.home && current_screen != Screen::Home {
            current_screen = Screen::Home;
            needs_redraw = true;
        }

        // the auxiliary button sleeps from any screen; the current screen is
        // restored on wake.
        if input.buttons.auxiliary {
            break;
        }

        if let Some(state) = input.touch {
            if let Some(point) = state.first_point() {
                let (sx, sy) = touch_to_screen(point.x, point.y);

                match current_screen {
                    Screen::Home => {
                        if let Some(idx) = hit_test(sx, sy) {
                            current_screen = ICONS[idx].screen;
                            needs_redraw = true;
                        }
                    }
                    Screen::Frontlight => {
                        if back_button_hit(sx, sy) {
                            current_screen = Screen::Home;
                            needs_redraw = true;
                        } else if minus_hit(sx, sy) {
                            brightness = brightness.saturating_sub(BRIGHTNESS_STEP);
                            light.set_brightness(brightness);
                            draw_brightness_area(&mut display, brightness);
                            display.flush_partial_fast(brightness_native_rect()).ok();
                        } else if plus_hit(sx, sy) {
                            brightness = brightness.saturating_add(BRIGHTNESS_STEP).min(100);
                            light.set_brightness(brightness);
                            draw_brightness_area(&mut display, brightness);
                            display.flush_partial_fast(brightness_native_rect()).ok();
                        }
                    }
                    Screen::Sleep => {
                        if back_button_hit(sx, sy) {
                            current_screen = Screen::Home;
                            needs_redraw = true;
                        } else if sleep_now_hit(sx, sy) {
                            // leave the loop to draw the screensaver and enter
                            // deep sleep below
                            break;
                        }
                    }
                    _ => {
                        if back_button_hit(sx, sy) {
                            current_screen = Screen::Home;
                            needs_redraw = true;
                        }
                    }
                }

                delay.delay_millis(300);
            }
        }

        // keep the GPS UART drained with a single non-blocking read per pass
        // and refresh the readout periodically. one read every ~50ms keeps
        // the 128-byte FIFO (~133ms at 9600 baud) from overflowing without
        // blocking the touch poll above.
        #[cfg(feature = "gps")]
        if let Some(ref mut g) = gps {
            g.update().ok();

            if current_screen == Screen::Gps && !needs_redraw {
                gps_refresh += 1;
                if gps_refresh >= GPS_REFRESH_TICKS {
                    gps_refresh = 0;
                    draw_gps_data(&mut display, g);
                    display.flush_partial_fast(gps_data_native_rect()).ok();
                }
            }
        }

        delay.delay_millis(50);
    }

    // sleep was requested from the Sleep screen. turn the front light off,
    // paint the screensaver (e-ink retains it with the panel unpowered), then
    // enter deep sleep. the boot button wakes the chip, which resets and
    // re-runs main() from the top.
    light.set_brightness(0);
    // remember where we were so wake lands on the same screen. single-threaded,
    // so writing the RTC-backed static is sound.
    unsafe {
        LAST_SCREEN = current_screen.to_index();
    }
    display.clear().ok();
    // pick a random wallpaper from the SD card; fall back to the drawn
    // screensaver if the folder is missing or has no usable .bmp files.
    if !show_wallpaper(
        &mut display,
        peripherals.SPI2,
        sdcard_pin_config!(peripherals),
    ) {
        let pct = display.battery_percentage().unwrap_or(0);
        draw_screensaver(&mut display, pct);
    }
    display.flush(DrawMode::BlackOnWhite).expect("to flush");
    display.deep_sleep(peripherals.LPWR, None)
}
