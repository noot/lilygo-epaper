#![no_std]
#![no_main]

extern crate alloc;
extern crate t5s3_epaper_core;

use core::format_args;

use embedded_graphics::prelude::*;
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use esp_backtrace as _;
use esp_hal::{delay::Delay, main};
use t5s3_epaper_core::{display::Rectangle, pin_config, Display, FrontLight};
use u8g2_fonts::FontRenderer;

static FONT: FontRenderer = FontRenderer::new::<u8g2_fonts::fonts::u8g2_font_spleen16x32_mr>();
static FONT_SMALL: FontRenderer =
    FontRenderer::new::<u8g2_fonts::fonts::u8g2_font_spleen12x24_mr>();

esp_bootloader_esp_idf::esp_app_desc!();

const STEP: u8 = 5;

#[main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();

    let config = esp_hal::Config::default();
    let config = config.with_cpu_clock(esp_hal::clock::CpuClock::_240MHz);
    let peripherals = esp_hal::init(config);

    esp_alloc::psram_allocator!(peripherals.PSRAM, esp_hal::psram);

    let i2c_worker = t5s3_epaper_core::i2c::Worker::new(
        peripherals.I2C0,
        peripherals.GPIO39,
        peripherals.GPIO40,
        t5s3_epaper_core::touch_pin_config!(peripherals),
    )
    .expect("to build i2c worker");
    static mut I2C_CORE_STACK: esp_hal::system::Stack<16384> = esp_hal::system::Stack::new();
    let mut cpu_control = esp_hal::system::CpuControl::new(peripherals.CPU_CTRL);
    let i2c_core_guard = cpu_control
        .start_app_core(
            unsafe { &mut *core::ptr::addr_of_mut!(I2C_CORE_STACK) },
            move || i2c_worker.run(),
        )
        .expect("to start the i2c worker on the second core");
    core::mem::forget(i2c_core_guard);

    let mut display = Display::new(
        pin_config!(peripherals),
        peripherals.DMA_CH0,
        peripherals.LCD_CAM,
        peripherals.RMT,
    )
    .expect("to initialize display");

    let mut light =
        FrontLight::new(peripherals.LEDC, peripherals.GPIO11).expect("to initialize front light");

    let delay = Delay::new();

    display.power_on().expect("to power on display");
    delay.delay_millis(10);
    display.clear().expect("to clear screen");

    let midpoint = Display::HEIGHT / 2;

    FONT_SMALL
        .render_aligned(
            "touch top half: brighter  |  touch bottom half: dimmer",
            Point::new(Display::WIDTH as i32 / 2, 30),
            u8g2_fonts::types::VerticalPosition::Baseline,
            u8g2_fonts::types::HorizontalAlignment::Center,
            u8g2_fonts::types::FontColor::WithBackground {
                fg: Gray4::BLACK,
                bg: Gray4::WHITE,
            },
            &mut display,
        )
        .expect("to render instructions");

    display
        .flush_partial_fast(Rectangle {
            x: 0,
            y: 0,
            width: Display::WIDTH,
            height: 50,
        })
        .expect("to flush instructions");

    let text_area = Rectangle {
        x: 300,
        y: 220,
        width: 360,
        height: 100,
    };

    let mut last_brightness: u8 = 0;
    render_brightness(&mut display, &text_area, last_brightness);

    loop {
        while let Some(event) = t5s3_epaper_core::i2c::poll_event() {
            if let t5s3_epaper_core::i2c::Event::Tap { y, .. } = event {
                let current = light.brightness();
                let new_brightness = if y < midpoint {
                    current.saturating_add(STEP).min(100)
                } else {
                    current.saturating_sub(STEP)
                };

                if new_brightness != current {
                    light.set_brightness(new_brightness);
                }

                if new_brightness != last_brightness {
                    render_brightness(&mut display, &text_area, new_brightness);
                    last_brightness = new_brightness;
                }
            }
        }

        delay.delay_millis(150);
    }
}

fn render_brightness(display: &mut Display, area: &Rectangle, pct: u8) {
    FONT.render_aligned(
        format_args!("brightness: {}%", pct),
        Point::new(
            area.x as i32 + area.width as i32 / 2,
            area.y as i32 + area.height as i32 / 2,
        ),
        u8g2_fonts::types::VerticalPosition::Center,
        u8g2_fonts::types::HorizontalAlignment::Center,
        u8g2_fonts::types::FontColor::WithBackground {
            fg: Gray4::BLACK,
            bg: Gray4::WHITE,
        },
        display,
    )
    .expect("to render brightness text");

    display
        .flush_partial_fast(*area)
        .expect("to flush brightness text");
}
