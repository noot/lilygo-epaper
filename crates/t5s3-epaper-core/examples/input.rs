#![no_std]
#![no_main]

extern crate alloc;
extern crate t5s3_epaper_core;

use core::format_args;

use embedded_graphics::prelude::*;
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use esp_backtrace as _;
use esp_hal::{
    delay::Delay,
    gpio::{Input, InputConfig, Pull},
    main,
};
use t5s3_epaper_core::{display::Rectangle, pin_config, Display};
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
    // the boot button is a plain GPIO with no i2c involvement, so it's read
    // directly here rather than through the i2c worker.
    let boot_btn = Input::new(
        peripherals.GPIO0,
        InputConfig::default().with_pull(Pull::Up),
    );

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

    // home presses and taps arrive pre-edge-detected from the i2c worker's
    // autonomous touch poll on the second core; `last_home` just latches one
    // frame so a press is visible before this loop clears it again.
    let mut last_home = false;
    let mut last_tap: Option<(u16, u16)> = None;

    loop {
        while let Some(event) = t5s3_epaper_core::i2c::poll_event() {
            match event {
                t5s3_epaper_core::i2c::Event::Home => last_home = true,
                t5s3_epaper_core::i2c::Event::Tap { x, y } => last_tap = Some((x, y)),
            }
        }
        let aux = t5s3_epaper_core::i2c::aux_button_pressed();
        let boot = boot_btn.is_low();

        FONT.render_aligned(
            format_args!(
                "Home: {}\nAux:  {}\nBoot: {}\nLast tap: {:?}",
                if last_home { "pressed " } else { "-       " },
                if aux { "pressed " } else { "released" },
                if boot { "pressed " } else { "released" },
                last_tap,
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

        last_home = false;
        delay.delay_millis(100);
    }
}
