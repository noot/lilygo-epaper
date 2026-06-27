use alloc::{format, string::String, vec::Vec};

use embedded_graphics::{
    image::Image,
    mono_font::{
        ascii::{FONT_9X15, FONT_9X18_BOLD},
        MonoTextStyle,
    },
    prelude::*,
    primitives::{PrimitiveStyle, PrimitiveStyleBuilder, Rectangle, RoundedRectangle},
    text::{Alignment, Text},
};
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use esp_hal::gpio::{Level, Output, OutputConfig};
use t5s3_epaper_core::{
    sdcard::{DirectoryEntry, Error, PinConfig},
    Display,
    SdCard,
};
use tinybmp::Bmp;

use crate::{
    layout::{screen_to_native_rect, SCREEN_W},
    widgets::draw_back_button,
};

const TITLE_Y: i32 = 95;
const PATH_Y: i32 = 140;
const LIST_X: i32 = 20;
const LIST_W: i32 = 500;
const LIST_TOP: i32 = 170;
const ROW_H: i32 = 42;
pub(crate) const VISIBLE_ROWS: usize = 14;
const LIST_H: i32 = ROW_H * VISIBLE_ROWS as i32;
const FOOTER_Y: i32 = LIST_TOP + LIST_H + 22;
const SCROLL_Y: i32 = FOOTER_Y + 14;
const SCROLL_BTN_W: u32 = 200;
const SCROLL_BTN_H: u32 = 80;
const UP_BTN_X: i32 = 40;
const DOWN_BTN_X: i32 = 300;

pub(crate) enum Row {
    Parent,
    Entry(usize),
}

// mount the SD card and list a directory, sorted directories-first then by
// name. the card shares SPI2 and the sclk/mosi/miso lines with the LoRa radio,
// so the pins are stolen (mirroring `make_radio`) and the radio chip-select is
// driven high to release MISO for the duration. the card and the CS guard drop
// when this returns, freeing the bus for the next access.
pub(crate) fn load_dir(path: &str) -> Result<Vec<DirectoryEntry>, Error> {
    let _lora_cs = Output::new(
        unsafe { esp_hal::peripherals::GPIO46::steal() },
        Level::High,
        OutputConfig::default(),
    );
    let pins = PinConfig {
        miso: unsafe { esp_hal::peripherals::GPIO21::steal() },
        mosi: unsafe { esp_hal::peripherals::GPIO13::steal() },
        sclk: unsafe { esp_hal::peripherals::GPIO14::steal() },
        cs: unsafe { esp_hal::peripherals::GPIO12::steal() },
    };
    let spi = unsafe { esp_hal::peripherals::SPI2::steal() };
    let card = SdCard::new(pins, spi)?;
    let mut entries = card.list_dir(path)?;
    entries.sort_by(|a, b| {
        b.is_directory
            .cmp(&a.is_directory)
            .then_with(|| a.name.as_str().cmp(b.name.as_str()))
    });
    Ok(entries)
}

pub(crate) fn is_bmp(name: &str) -> bool {
    name.rsplit_once('.')
        .is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("bmp"))
}

// read a grayscale .bmp from the card and draw it centered on the screen.
// mounts the card the same self-contained way as `load_dir`. returns false if
// the card, file, or bitmap is missing or unreadable so the caller can show a
// message.
pub(crate) fn view_image(display: &mut Display, path: &str) -> bool {
    let _lora_cs = Output::new(
        unsafe { esp_hal::peripherals::GPIO46::steal() },
        Level::High,
        OutputConfig::default(),
    );
    let pins = PinConfig {
        miso: unsafe { esp_hal::peripherals::GPIO21::steal() },
        mosi: unsafe { esp_hal::peripherals::GPIO13::steal() },
        sclk: unsafe { esp_hal::peripherals::GPIO14::steal() },
        cs: unsafe { esp_hal::peripherals::GPIO12::steal() },
    };
    let spi = unsafe { esp_hal::peripherals::SPI2::steal() };
    let card = match SdCard::new(pins, spi) {
        Ok(card) => card,
        Err(e) => {
            esp_println::println!("files: view sd init failed: {e:?}");
            return false;
        }
    };
    let bytes = match card.read_file(path) {
        Ok(bytes) => bytes,
        Err(e) => {
            esp_println::println!("files: read {path} failed: {e:?}");
            return false;
        }
    };
    let Ok(bmp) = Bmp::<Gray4>::from_slice(&bytes) else {
        esp_println::println!("files: parse {path} failed");
        return false;
    };
    let screen = display.bounding_box().size;
    let image = bmp.size();
    let x = (screen.width as i32 - image.width as i32) / 2;
    let y = (screen.height as i32 - image.height as i32) / 2;
    Image::new(&bmp, Point::new(x.max(0), y.max(0)))
        .draw(display)
        .is_ok()
}

pub(crate) fn parent_path(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(0) | None => String::from("/"),
        Some(i) => String::from(&trimmed[..i]),
    }
}

pub(crate) fn display_row_count(path: &str, entry_count: usize) -> usize {
    entry_count + usize::from(!is_root(path))
}

fn is_root(path: &str) -> bool {
    path.trim_matches('/').is_empty()
}

fn truncate_tail(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        return String::from(s);
    }
    let tail: String = s.chars().skip(count - (max - 3)).collect();
    format!("...{tail}")
}

fn human_size(bytes: u32) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{} KB", bytes / 1024)
    } else {
        format!("{} MB", bytes / (1024 * 1024))
    }
}

pub(crate) fn list_hit(
    sx: i32,
    sy: i32,
    path: &str,
    entry_count: usize,
    scroll: usize,
) -> Option<Row> {
    if !(LIST_X..LIST_X + LIST_W).contains(&sx) || !(LIST_TOP..LIST_TOP + LIST_H).contains(&sy) {
        return None;
    }
    let slot = ((sy - LIST_TOP) / ROW_H) as usize;
    if slot >= VISIBLE_ROWS {
        return None;
    }
    let di = scroll + slot;
    if di >= display_row_count(path, entry_count) {
        return None;
    }
    let root = is_root(path);
    if !root && di == 0 {
        Some(Row::Parent)
    } else {
        Some(Row::Entry(di - usize::from(!root)))
    }
}

pub(crate) fn files_scroll_up_hit(sx: i32, sy: i32) -> bool {
    (UP_BTN_X..UP_BTN_X + SCROLL_BTN_W as i32).contains(&sx)
        && (SCROLL_Y..SCROLL_Y + SCROLL_BTN_H as i32).contains(&sy)
}

pub(crate) fn files_scroll_down_hit(sx: i32, sy: i32) -> bool {
    (DOWN_BTN_X..DOWN_BTN_X + SCROLL_BTN_W as i32).contains(&sx)
        && (SCROLL_Y..SCROLL_Y + SCROLL_BTN_H as i32).contains(&sy)
}

fn draw_button(display: &mut Display, x: i32, label: &str) {
    let border = PrimitiveStyleBuilder::new()
        .stroke_color(Gray4::BLACK)
        .stroke_width(2)
        .fill_color(Gray4::WHITE)
        .build();
    RoundedRectangle::with_equal_corners(
        Rectangle::new(
            Point::new(x, SCROLL_Y),
            Size::new(SCROLL_BTN_W, SCROLL_BTN_H),
        ),
        Size::new(10, 10),
    )
    .into_styled(border)
    .draw(display)
    .ok();
    Text::with_alignment(
        label,
        Point::new(
            x + SCROLL_BTN_W as i32 / 2,
            SCROLL_Y + SCROLL_BTN_H as i32 / 2 + 6,
        ),
        MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK),
        Alignment::Center,
    )
    .draw(display)
    .ok();
}

fn draw_path(display: &mut Display, path: &str) {
    Rectangle::new(
        Point::new(LIST_X, PATH_Y - 16),
        Size::new(LIST_W as u32, 24),
    )
    .into_styled(PrimitiveStyle::with_fill(Gray4::WHITE))
    .draw(display)
    .ok();
    Text::new(
        &truncate_tail(path, 56),
        Point::new(LIST_X, PATH_Y),
        MonoTextStyle::new(&FONT_9X15, Gray4::new(4)),
    )
    .draw(display)
    .ok();
}

pub(crate) fn draw_file_list(
    display: &mut Display,
    path: &str,
    entries: &[DirectoryEntry],
    scroll: usize,
) {
    Rectangle::new(
        Point::new(LIST_X, LIST_TOP),
        Size::new(LIST_W as u32, LIST_H as u32),
    )
    .into_styled(PrimitiveStyle::with_fill(Gray4::WHITE))
    .draw(display)
    .ok();

    let font = MonoTextStyle::new(&FONT_9X15, Gray4::BLACK);
    let root = is_root(path);
    let total = display_row_count(path, entries.len());

    for slot in 0..VISIBLE_ROWS {
        let di = scroll + slot;
        if di >= total {
            break;
        }
        let y = LIST_TOP + slot as i32 * ROW_H + 28;
        if !root && di == 0 {
            Text::new(".. (up one level)", Point::new(LIST_X + 8, y), font)
                .draw(display)
                .ok();
            continue;
        }
        let entry = &entries[di - usize::from(!root)];
        if entry.is_directory {
            let label = format!("{}/", truncate_tail(&entry.name, 48));
            Text::new(&label, Point::new(LIST_X + 8, y), font)
                .draw(display)
                .ok();
        } else {
            Text::new(
                &truncate_tail(&entry.name, 40),
                Point::new(LIST_X + 8, y),
                font,
            )
            .draw(display)
            .ok();
            Text::with_alignment(
                &human_size(entry.size),
                Point::new(LIST_X + LIST_W, y),
                font,
                Alignment::Right,
            )
            .draw(display)
            .ok();
        }
    }
}

pub(crate) fn draw_files_footer(display: &mut Display, status: &str) {
    Rectangle::new(
        Point::new(LIST_X, FOOTER_Y - 16),
        Size::new(LIST_W as u32, 24),
    )
    .into_styled(PrimitiveStyle::with_fill(Gray4::WHITE))
    .draw(display)
    .ok();
    Text::new(
        status,
        Point::new(LIST_X, FOOTER_Y),
        MonoTextStyle::new(&FONT_9X15, Gray4::BLACK),
    )
    .draw(display)
    .ok();
}

pub(crate) fn draw_files_screen(
    display: &mut Display,
    path: &str,
    entries: &[DirectoryEntry],
    scroll: usize,
    status: &str,
) {
    draw_back_button(display);
    Text::with_alignment(
        "Files",
        Point::new(SCREEN_W / 2, TITLE_Y),
        MonoTextStyle::new(&FONT_9X18_BOLD, Gray4::BLACK),
        Alignment::Center,
    )
    .draw(display)
    .ok();
    draw_path(display, path);
    draw_file_list(display, path, entries, scroll);
    draw_files_footer(display, status);
    draw_button(display, UP_BTN_X, "Up");
    draw_button(display, DOWN_BTN_X, "Down");
}

pub(crate) fn file_list_native_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(LIST_X, LIST_TOP, LIST_W, LIST_H)
}

pub(crate) fn files_footer_native_rect() -> t5s3_epaper_core::display::Rectangle {
    screen_to_native_rect(LIST_X, FOOTER_Y - 16, LIST_W, 24)
}
