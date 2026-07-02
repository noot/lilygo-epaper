use alloc::vec::Vec;
use core::{cell::RefCell, fmt::Write as _};

use embedded_graphics::{
    image::Image,
    mono_font::{
        ascii::{FONT_9X15, FONT_9X18_BOLD},
        MonoTextStyle,
    },
    prelude::*,
    primitives::{Circle, PrimitiveStyle, PrimitiveStyleBuilder, Rectangle, RoundedRectangle},
    text::{Alignment, Text},
};
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use esp_hal::{
    gpio::{Level, Output, OutputConfig},
    rng::Rng,
    spi::master::Spi,
    time::Instant,
    Blocking,
};
use t5s3_epaper_core::{Display, SdCard};
use tinybmp::Bmp;

use crate::{fmt::FmtBuf, layout::SCREEN_W, widgets::draw_back_button};

const SLEEP_BTN_X: i32 = 160;
const SLEEP_BTN_Y: i32 = 400;
const SLEEP_BTN_W: u32 = 220;
const SLEEP_BTN_H: u32 = 90;
const POWER_BTN_Y: i32 = 530;

// folder of 540x960 grayscale .bmp wallpapers on the SD card; one is picked at
// random as the sleep screensaver. this must be a FAT 8.3 name (<=8 chars), as
// must the .bmp files inside it, so they can be opened by name.
const WALLPAPER_DIR: &str = "/WALLS";

pub(crate) fn sleep_now_hit(sx: i32, sy: i32) -> bool {
    (SLEEP_BTN_X..SLEEP_BTN_X + SLEEP_BTN_W as i32).contains(&sx)
        && (SLEEP_BTN_Y..SLEEP_BTN_Y + SLEEP_BTN_H as i32).contains(&sy)
}

pub(crate) fn power_off_hit(sx: i32, sy: i32) -> bool {
    (SLEEP_BTN_X..SLEEP_BTN_X + SLEEP_BTN_W as i32).contains(&sx)
        && (POWER_BTN_Y..POWER_BTN_Y + SLEEP_BTN_H as i32).contains(&sy)
}

pub(crate) fn draw_sleep_screen(display: &mut Display) {
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

    button(display, SLEEP_BTN_Y, "Sleep Now");
    button(display, POWER_BTN_Y, "Power Off");
}

// a labelled button spanning the shared sleep-screen button column at `y`.
fn button(display: &mut Display, y: i32, label: &str) {
    let bold = MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK);
    let border = PrimitiveStyleBuilder::new()
        .stroke_color(Gray4::BLACK)
        .stroke_width(3)
        .fill_color(Gray4::WHITE)
        .build();
    RoundedRectangle::with_equal_corners(
        Rectangle::new(
            Point::new(SLEEP_BTN_X, y),
            Size::new(SLEEP_BTN_W, SLEEP_BTN_H),
        ),
        Size::new(12, 12),
    )
    .into_styled(border)
    .draw(display)
    .ok();
    Text::with_alignment(
        label,
        Point::new(
            SLEEP_BTN_X + SLEEP_BTN_W as i32 / 2,
            y + SLEEP_BTN_H as i32 / 2 + 6,
        ),
        bold,
        Alignment::Center,
    )
    .draw(display)
    .ok();
}

// shown while in deep sleep, when no SD wallpaper is available.
pub(crate) fn draw_screensaver(display: &mut Display, pct: u16) {
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

// shown right before the PMIC powers the board off (battery FET disabled).
pub(crate) fn draw_power_off_screen(display: &mut Display, pct: u16) {
    let bold = MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK);
    let small = MonoTextStyle::new(&FONT_9X15, Gray4::BLACK);
    let cx = SCREEN_W / 2;

    Text::with_alignment("Powered Off", Point::new(cx, 420), bold, Alignment::Center)
        .draw(display)
        .ok();
    Text::with_alignment(
        "Press the power button to turn on.",
        Point::new(cx, 470),
        small,
        Alignment::Center,
    )
    .draw(display)
    .ok();

    let mut buf = FmtBuf::<24>::new();
    write!(buf, "Battery {}%", pct.min(100)).ok();
    Text::with_alignment(buf.as_str(), Point::new(cx, 540), small, Alignment::Center)
        .draw(display)
        .ok();
}

// load the wallpaper bitmap from the SD card and draw it full-screen. returns
// false if the card, file, or bitmap is missing or unreadable so the caller can
// fall back to the drawn screensaver.
pub(crate) fn show_wallpaper(display: &mut Display, bus: &RefCell<Spi<'static, Blocking>>) -> bool {
    // the SD card shares the bus with the LoRa SX1262 radio, which is dropped
    // while off its screen; its chip-select floats, so hold it high to make the
    // idle radio release MISO (otherwise SD init returns CardNotFound).
    let _lora_cs = Output::new(
        unsafe { esp_hal::peripherals::GPIO46::steal() },
        Level::High,
        OutputConfig::default(),
    );

    let sdcard = match SdCard::new(unsafe { esp_hal::peripherals::GPIO12::steal() }, bus) {
        Ok(sdcard) => sdcard,
        Err(e) => {
            esp_println::println!("wallpaper: sd init failed: {e:?}");
            return false;
        }
    };
    let entries = match sdcard.list_dir(WALLPAPER_DIR) {
        Ok(entries) => entries,
        Err(e) => {
            esp_println::println!("wallpaper: list_dir {WALLPAPER_DIR} failed: {e:?}");
            return false;
        }
    };

    let mut paths = Vec::new();
    for entry in entries {
        if !entry.is_directory && is_bmp(&entry.name) {
            paths.push(entry.path);
        }
    }
    if paths.is_empty() {
        esp_println::println!("wallpaper: no .bmp files in {WALLPAPER_DIR}");
        return false;
    }

    // mix the hardware RNG with a microsecond timer reading. with the radios
    // off the RNG alone is biased (it kept picking the same file); the instant
    // at which sleep is triggered adds real entropy. then fall through the rest
    // so an unreadable file (e.g. a long name the FAT layer can't open by its
    // 8.3 short name) is skipped rather than aborting.
    let seed = Rng::new().random() ^ Instant::now().duration_since_epoch().as_micros() as u32;
    let start = seed as usize % paths.len();
    for offset in 0..paths.len() {
        let path = &paths[(start + offset) % paths.len()];
        let bytes = match sdcard.read_file(path) {
            Ok(bytes) => bytes,
            Err(e) => {
                esp_println::println!("wallpaper: read {path} failed: {e:?}");
                continue;
            }
        };
        let Ok(bmp) = Bmp::<Gray4>::from_slice(&bytes) else {
            esp_println::println!("wallpaper: parse {path} failed");
            continue;
        };
        if Image::new(&bmp, Point::zero()).draw(display).is_ok() {
            esp_println::println!("wallpaper: drew {path}");
            return true;
        }
    }
    false
}

fn is_bmp(name: &str) -> bool {
    name.rsplit_once('.')
        .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("bmp"))
}
