#![no_std]
#![no_main]

extern crate t5s3_epaper_core;

use embedded_graphics::{prelude::*, primitives::PrimitiveStyleBuilder};
use embedded_graphics_core::{
    geometry::Point,
    pixelcolor::Gray4,
    prelude::Dimensions,
    primitives::Rectangle,
};
#[allow(unused_imports)]
use esp_backtrace as _;
use esp_hal::{delay::Delay, main};
use t5s3_epaper_core::{pin_config, Display, DrawMode};

esp_bootloader_esp_idf::esp_app_desc!();

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
    .expect("to initialize correctly");

    let delay = Delay::new();
    display.power_on().expect("to power on display");
    delay.delay_millis(10);
    display.clear().expect("to clear display");

    loop {
        let height = display.bounding_box().size.height / 16;
        for shade in 0x0..0x0F {
            Rectangle::new(
                Point::new(0, height as i32 * shade as i32),
                Size::new(display.bounding_box().size.width, height),
            )
            .into_styled(
                PrimitiveStyleBuilder::new()
                    .fill_color(Gray4::new(shade))
                    .build(),
            )
            .draw(&mut display)
            .expect("to draw in framebuffer");
        }

        display
            .flush(DrawMode::BlackOnWhite)
            .expect("to flush to display");

        delay.delay_millis(5000);

        display.clear().expect("to clear display");

        let width = display.bounding_box().size.width / 16;
        for shade in 0x0..0x0F {
            Rectangle::new(
                Point::new(width as i32 * shade as i32, 0),
                Size::new(width, display.bounding_box().size.height),
            )
            .into_styled(
                PrimitiveStyleBuilder::new()
                    .fill_color(Gray4::new(shade))
                    .build(),
            )
            .draw(&mut display)
            .expect("to draw in framebuffer");
        }

        display
            .flush(DrawMode::BlackOnWhite)
            .expect("to flush to display");

        delay.delay_millis(5000);

        display.clear().expect("to clear display");
    }
}
