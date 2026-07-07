#![no_std]
#![no_main]

extern crate alloc;
extern crate t5s3_epaper_core;

use core::format_args;

use embedded_graphics::prelude::*;
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use esp_backtrace as _;
use esp_hal::{delay::Delay, main};
use t5s3_epaper_core::{
    display::Rectangle,
    gps::Gps,
    gps_pin_config,
    pin_config,
    Display,
    DrawMode,
};
use u8g2_fonts::FontRenderer;

static FONT: FontRenderer = FontRenderer::new::<u8g2_fonts::fonts::u8g2_font_spleen12x24_mr>();

esp_bootloader_esp_idf::esp_app_desc!();

#[main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();

    let config = esp_hal::Config::default();
    let config = config.with_cpu_clock(esp_hal::clock::CpuClock::_240MHz);
    let peripherals = esp_hal::init(config);

    esp_alloc::psram_allocator!(peripherals.PSRAM, esp_hal::psram);

    let i2c_bus =
        t5s3_epaper_core::i2c::Bus::new(peripherals.I2C0, peripherals.GPIO39, peripherals.GPIO40)
            .expect("to build i2c bus");
    let mut display = Display::new(
        pin_config!(peripherals),
        &i2c_bus,
        peripherals.DMA_CH0,
        peripherals.LCD_CAM,
        peripherals.RMT,
    )
    .expect("to initialize display");

    let mut delay = Delay::new();

    display.power_on().expect("to power on display");
    delay.delay_millis(10);
    display.clear().expect("to clear screen");

    FONT.render_aligned(
        "detecting GPS module...",
        Point::new(Display::WIDTH as i32 / 2, 30),
        u8g2_fonts::types::VerticalPosition::Baseline,
        u8g2_fonts::types::HorizontalAlignment::Center,
        u8g2_fonts::types::FontColor::WithBackground {
            fg: Gray4::BLACK,
            bg: Gray4::WHITE,
        },
        &mut display,
    )
    .expect("to render text");

    display
        .flush(DrawMode::BlackOnWhite)
        .expect("to flush display");

    let mut gps = Gps::detect(peripherals.UART1, gps_pin_config!(peripherals), &mut delay)
        .expect("to detect and initialize GPS");

    esp_println::println!("detected GPS module: {:?}", gps.module());

    display.clear().expect("to clear screen");

    FONT.render_aligned(
        format_args!("GPS module: {:?}", gps.module()),
        Point::new(Display::WIDTH as i32 / 2, 30),
        u8g2_fonts::types::VerticalPosition::Baseline,
        u8g2_fonts::types::HorizontalAlignment::Center,
        u8g2_fonts::types::FontColor::WithBackground {
            fg: Gray4::BLACK,
            bg: Gray4::WHITE,
        },
        &mut display,
    )
    .expect("to render text");

    display
        .flush(DrawMode::BlackOnWhite)
        .expect("to flush display");

    let text_area = Rectangle {
        x: 40,
        y: 60,
        width: 880,
        height: 400,
    };

    loop {
        // Drain the UART continuously for ~1 s before touching the slow
        // e-paper. The 128-byte RX FIFO holds barely 130 ms of data at
        // 9600 baud, so reading only once per refresh loses whole sentences
        // and a fix never assembles.
        for _ in 0..50 {
            if let Err(e) = gps.update() {
                esp_println::println!("gps update error: {}", e);
            }
            delay.delay_millis(20);
        }

        let fix_str = match gps.fix_type() {
            Some(nmea::sentences::FixType::Gps) => "GPS",
            Some(nmea::sentences::FixType::DGps) => "DGPS",
            Some(nmea::sentences::FixType::Rtk) => "RTK",
            Some(nmea::sentences::FixType::FloatRtk) => "FloatRTK",
            Some(_) => "other",
            None => "no fix",
        };

        let location = gps.location();
        let status = if location.is_some() {
            "fix acquired"
        } else {
            "searching for fix..."
        };

        let (lat, lng) = location.unwrap_or((0.0, 0.0));
        let alt = gps.altitude().unwrap_or(0.0);
        let speed = gps.speed_over_ground().unwrap_or(0.0);
        let sats = gps.fix_satellites().unwrap_or(0);
        let hdop = gps.hdop().unwrap_or(0.0);
        let vdop = gps.vdop().unwrap_or(0.0);
        FONT.render_aligned(
            format_args!(
                "{:<20}\n\nfix:   {}\nsats:  {}\nhdop:  {:.1}\nvdop:  {:.1}\nlat:   {:.6}\nlng:   {:.6}\nalt:   {:.1} m\nspeed: {:.1} kn",
                status, fix_str, sats, hdop, vdop, lat, lng, alt, speed
            ),
            Point::new(60, 100),
            u8g2_fonts::types::VerticalPosition::Baseline,
            u8g2_fonts::types::HorizontalAlignment::Left,
            u8g2_fonts::types::FontColor::WithBackground {
                fg: Gray4::BLACK,
                bg: Gray4::WHITE,
            },
            &mut display,
        )
        .expect("to render GPS data");

        display
            .flush_partial_fast(text_area)
            .expect("to flush GPS data");
    }
}
