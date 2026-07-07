#![no_std]
#![no_main]

extern crate alloc;
extern crate t5s3_epaper_core;

use core::{format_args, time::Duration};

use embedded_graphics::prelude::*;
use embedded_graphics_core::{
    pixelcolor::{Gray4, GrayColor},
    primitives::Rectangle,
};
use esp_backtrace as _;
use esp_hal::{delay::Delay, main, ram};
use t5s3_epaper_core::{pin_config, power, Display, DrawMode};
use u8g2_fonts::FontRenderer;

static FONT: FontRenderer = FontRenderer::new::<u8g2_fonts::fonts::u8g2_font_spleen16x32_mr>();

esp_bootloader_esp_idf::esp_app_desc!();

#[ram(unstable(rtc_fast))]
static mut CYCLE: u16 = 0;

#[ram(unstable(rtc_fast))]
static mut LAST_RECT: Rectangle = Rectangle {
    top_left: Point { x: 0, y: 0 },
    size: Size {
        width: 0,
        height: 0,
    },
};

#[main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();

    let config = esp_hal::Config::default();
    let config = config.with_cpu_clock(esp_hal::clock::CpuClock::_240MHz);
    let peripherals = esp_hal::init(config);

    // Create PSRAM allocator
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
    let input_ctl = t5s3_epaper_core::input::Controller::new(
        &i2c_bus,
        t5s3_epaper_core::input_pin_config!(peripherals),
    );

    let delay = Delay::new();
    let wake = power::wake_status();

    // turn screen on
    display.power_on().expect("to power on display");
    delay.delay_millis(20);
    // clear
    let cycle = unsafe { CYCLE };
    let last_rect = unsafe { LAST_RECT };

    if cycle > 0 && cycle % 5 != 0 {
        display
            .fill_solid(&last_rect, Gray4::WHITE)
            .expect("to draw rectangle to framebuffer");
        display
            .flush(DrawMode::WhiteOnBlack)
            .expect("to flush to display");
    } else {
        display.clear().unwrap();
    }
    // write out reset and wake reason
    let rect = FONT
        .render_aligned(
            format_args!(
                "Reset Reason: {:?}\nWake reason: {:?}\nCycle: {}\nRect: ({}, {}, {}, {})",
                wake.reset_reason,
                wake.wakeup_cause,
                cycle,
                last_rect.top_left.x,
                last_rect.top_left.y,
                last_rect.size.width,
                last_rect.size.height,
            ),
            Point::new(
                display.bounding_box().center().x,
                display.bounding_box().center().y,
            ),
            u8g2_fonts::types::VerticalPosition::Baseline,
            u8g2_fonts::types::HorizontalAlignment::Center,
            u8g2_fonts::types::FontColor::WithBackground {
                fg: Gray4::BLACK,
                bg: Gray4::WHITE,
            },
            &mut display,
        )
        .expect("to render font to framebuffer");
    display
        .flush(DrawMode::BlackOnWhite)
        .expect("to flush to display");
    // turn screen off
    display.power_off().expect("to power off display");
    unsafe {
        if let Some(rect) = rect {
            LAST_RECT = rect;
        }
        CYCLE += 1;
    }

    delay.delay_millis(100);

    display.deep_sleep(peripherals.LPWR, input_ctl, Some(Duration::from_secs(30)));
}
