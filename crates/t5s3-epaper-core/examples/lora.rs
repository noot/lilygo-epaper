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
    lora::{Config, Lora},
    lora_pin_config,
    pin_config,
    Display,
    DrawMode,
};
use u8g2_fonts::FontRenderer;

static FONT: FontRenderer = FontRenderer::new::<u8g2_fonts::fonts::u8g2_font_spleen12x24_mr>();

esp_bootloader_esp_idf::esp_app_desc!();

fn render(display: &mut Display, text: core::fmt::Arguments) {
    display.clear().expect("to clear screen");
    FONT.render_aligned(
        text,
        Point::new(60, 100),
        u8g2_fonts::types::VerticalPosition::Baseline,
        u8g2_fonts::types::HorizontalAlignment::Left,
        u8g2_fonts::types::FontColor::WithBackground {
            fg: Gray4::BLACK,
            bg: Gray4::WHITE,
        },
        display,
    )
    .expect("to render text");
    display
        .flush(DrawMode::BlackOnWhite)
        .expect("to flush display");
}

#[main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();

    let config = esp_hal::Config::default();
    let config = config.with_cpu_clock(esp_hal::clock::CpuClock::_240MHz);
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

    let delay = Delay::new();

    display.power_on().expect("to power on display");
    // LoRa and GPS share the VCC3V3 rail enabled by Display::new(); the
    // official firmware waits 1.5 s after power-on before talking to the radio.
    delay.delay_millis(1500);

    let config = Config::default();
    let bus = t5s3_epaper_core::spi::Bus::new(
        peripherals.SPI2,
        peripherals.GPIO14,
        peripherals.GPIO13,
        peripherals.GPIO21,
        peripherals.GPIO12,
        peripherals.GPIO46,
    )
    .expect("to build spi bus");
    let mut radio = match Lora::new(&bus, lora_pin_config!(peripherals), &config) {
        Ok(radio) => radio,
        Err(e) => {
            esp_println::println!("lora init failed: {}", e);
            render(
                &mut display,
                format_args!("LoRa SX1262\n\ninit FAILED:\n{}", e),
            );
            loop {
                delay.delay_millis(1000);
            }
        }
    };

    let status = radio.status();
    let errors = radio.device_errors();
    esp_println::println!(
        "lora comms OK - status: {:?}, device_errors: {:?}",
        status,
        errors
    );

    let status_val = status.unwrap_or(0);
    let errors_val = errors.unwrap_or(0);
    render(
        &mut display,
        format_args!(
            "LoRa SX1262\n\ncomms:  OK\nstatus: {:#04x}\nerrors: {:#06x}",
            status_val, errors_val
        ),
    );

    loop {
        delay.delay_millis(1000);
    }
}
