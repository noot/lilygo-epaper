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
use esp_hal::{delay::Delay, gpio::Pin, main, ram};
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

    display.deep_sleep(
        peripherals.LPWR,
        peripherals.GPIO0.degrade(),
        Some(Duration::from_secs(30)),
    );
}
