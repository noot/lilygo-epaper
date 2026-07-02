#![no_std]
#![no_main]

extern crate alloc;
extern crate t5s3_epaper_core;

use alloc::format;
use core::format_args;

use embedded_graphics::prelude::*;
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use esp_backtrace as _;
use esp_hal::{delay::Delay, main};
use t5s3_epaper_core::{
    display::Rectangle,
    lora::{Config, Lora},
    lora_pin_config,
    pin_config,
    Display,
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
    display.clear().expect("to clear screen");

    let bus = t5s3_epaper_core::sdcard::shared_bus(
        peripherals.SPI2,
        peripherals.GPIO14,
        peripherals.GPIO13,
        peripherals.GPIO21,
    )
    .expect("to build spi bus");
    let mut radio = Lora::new(&bus, lora_pin_config!(peripherals), &Config::default())
        .expect("to initialize LoRa radio");

    let text_area = Rectangle {
        x: 40,
        y: 60,
        width: 880,
        height: 200,
    };

    let mut counter: u32 = 0;
    loop {
        let payload = format!("ping {counter}");
        let result = radio.transmit(payload.as_bytes());
        match &result {
            Ok(()) => esp_println::println!("lora tx: {}", payload),
            Err(e) => esp_println::println!("lora tx error: {}", e),
        }

        let status = match result {
            Ok(()) => "sent",
            Err(_) => "ERROR",
        };
        FONT.render_aligned(
            format_args!(
                "LoRa TX @ 915 MHz\n\nstatus: {}\ncount:  {}\nlast:   {}",
                status, counter, payload
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
        .expect("to render tx status");
        display
            .flush_partial_fast(text_area)
            .expect("to flush tx status");

        counter = counter.wrapping_add(1);
        delay.delay_millis(2000);
    }
}
