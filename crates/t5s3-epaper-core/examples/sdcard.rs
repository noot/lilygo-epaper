#![no_std]
#![no_main]

extern crate alloc;
extern crate t5s3_epaper_core;

use alloc::string::String;
use core::{fmt::Write as _, format_args};

use embedded_graphics::prelude::*;
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use esp_backtrace as _;
use esp_hal::{delay::Delay, main};
use t5s3_epaper_core::{pin_config, Display, DrawMode, SdCard};
use u8g2_fonts::FontRenderer;

static FONT: FontRenderer = FontRenderer::new::<u8g2_fonts::fonts::u8g2_font_spleen16x32_mr>();

const TEST_DIR: &str = "/DRVTEST";
const NESTED_DIR: &str = "/DRVTEST/NESTED";
const SOURCE_FILE: &str = "/DRVTEST/NESTED/HELLO.TXT";
const RENAMED_FILE: &str = "/DRVTEST/RENAMED.TXT";

esp_bootloader_esp_idf::esp_app_desc!();

#[main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();

    let config = esp_hal::Config::default().with_cpu_clock(esp_hal::clock::CpuClock::_240MHz);
    let peripherals = esp_hal::init(config);
    let delay = Delay::new();

    esp_alloc::psram_allocator!(peripherals.PSRAM, esp_hal::psram);

    let mut display = Display::new(
        pin_config!(peripherals),
        peripherals.I2C0,
        peripherals.DMA_CH0,
        peripherals.LCD_CAM,
        peripherals.RMT,
    )
    .expect("to initialize display");

    let bus = t5s3_epaper_core::spi::Bus::new(
        peripherals.SPI2,
        peripherals.GPIO14,
        peripherals.GPIO13,
        peripherals.GPIO21,
        peripherals.GPIO12,
        peripherals.GPIO46,
    )
    .expect("to build sd bus");
    let sdcard = SdCard::new(&bus).expect("to initialize sd card");
    let card_size = sdcard.card_size_bytes();

    sdcard
        .create_dir_all(NESTED_DIR)
        .expect("to create nested directory tree");
    assert!(sdcard.exists(TEST_DIR).expect("to check top-level dir"));
    assert!(sdcard.exists(NESTED_DIR).expect("to check nested dir"));

    sdcard
        .write_file(SOURCE_FILE, b"hello")
        .expect("to write source file");
    sdcard
        .append_file(SOURCE_FILE, b" world")
        .expect("to append source file");

    let contents = sdcard.read_file(SOURCE_FILE).expect("to read source file");
    assert_eq!(contents.as_slice(), b"hello world");

    let metadata = sdcard.metadata(SOURCE_FILE).expect("to read metadata");
    assert_eq!(metadata.size, 11);
    assert!(!metadata.is_directory);

    let nested_listing = sdcard.list_dir(NESTED_DIR).expect("to list nested dir");
    assert!(nested_listing.iter().any(|entry| entry.name == "HELLO.TXT"));

    sdcard
        .rename_file(SOURCE_FILE, RENAMED_FILE)
        .expect("to rename file");
    assert!(!sdcard
        .exists(SOURCE_FILE)
        .expect("to verify source removal"));
    assert!(sdcard.exists(RENAMED_FILE).expect("to verify renamed file"));
    assert_eq!(
        sdcard
            .read_file(RENAMED_FILE)
            .expect("to read renamed file"),
        b"hello world"
    );

    sdcard
        .delete_file(RENAMED_FILE)
        .expect("to delete renamed file");
    assert!(!sdcard.exists(RENAMED_FILE).expect("to verify delete"));

    let remove_dir_result = sdcard.remove_dir(NESTED_DIR);
    assert!(matches!(
        remove_dir_result,
        Err(t5s3_epaper_core::sdcard::Error::Unsupported(_))
    ));

    let mut listing = sdcard.list_dir(TEST_DIR).expect("to list test dir");
    listing.sort_by(|a, b| a.name.cmp(&b.name));

    let mut body = String::new();
    let _ = writeln!(body, "SD: {} MB", card_size / (1024 * 1024));
    let _ = writeln!(body, "create_dir_all ok");
    let _ = writeln!(body, "write+append+read ok");
    let _ = writeln!(body, "metadata+list ok");
    let _ = writeln!(body, "rename+delete ok");
    let _ = writeln!(body, "remove_dir unsupported");
    let _ = writeln!(body);

    for entry in listing.iter().take(10) {
        let kind = if entry.is_directory { 'D' } else { 'F' };
        let _ = writeln!(body, "{} {:<16} {}", kind, entry.name, entry.size);
    }

    display.power_on().expect("to power on display");
    delay.delay_millis(20);
    display.clear().expect("to clear display");

    FONT.render_aligned(
        format_args!("{}", body),
        Point::new(24, 48),
        u8g2_fonts::types::VerticalPosition::Top,
        u8g2_fonts::types::HorizontalAlignment::Left,
        u8g2_fonts::types::FontColor::WithBackground {
            fg: Gray4::BLACK,
            bg: Gray4::WHITE,
        },
        &mut display,
    )
    .expect("to render text");

    display
        .flush(DrawMode::BlackOnWhite)
        .expect("to flush to display");
    display.power_off().expect("to power off display");

    loop {
        core::hint::spin_loop();
    }
}
