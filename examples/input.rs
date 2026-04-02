#![no_std]
#![no_main]

extern crate alloc;
extern crate lilygo_epd47;

use core::format_args;

use embedded_graphics::prelude::*;
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use esp_backtrace as _;
use esp_hal::{delay::Delay, main};
use lilygo_epd47::display::Rectangle;
use lilygo_epd47::{pin_config, Display};
use u8g2_fonts::FontRenderer;

static FONT: FontRenderer = FontRenderer::new::<u8g2_fonts::fonts::u8g2_font_spleen16x32_mr>();

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
    delay.delay_millis(10);
    display.clear().expect("to clear screen");

    let text_origin = Point::new(60, 180);
    let text_area = Rectangle {
        x: 40,
        y: 120,
        width: 880,
        height: 280,
    };

    loop {
        let input = display.input().expect("to read input state");

        FONT.render_aligned(
            format_args!(
                "Home: {}\nAux:  {}\nBoot: {}",
                if input.buttons.home { "pressed " } else { "released" },
                if input.buttons.auxiliary {
                    "pressed "
                } else {
                    "released"
                },
                if input.buttons.boot { "pressed " } else { "released" }
            ),
            text_origin,
            u8g2_fonts::types::VerticalPosition::Baseline,
            u8g2_fonts::types::HorizontalAlignment::Left,
            u8g2_fonts::types::FontColor::WithBackground {
                fg: Gray4::BLACK,
                bg: Gray4::WHITE,
            },
            &mut display,
        )
        .expect("to render input status");

        display
            .flush_partial_fast(text_area)
            .expect("to flush input status");

        delay.delay_millis(100);
    }
}
