#![no_std]
#![no_main]

extern crate alloc;
extern crate t5s3_epaper_core;

use embedded_graphics::{
    prelude::*,
    primitives::{Circle, PrimitiveStyle},
};
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use esp_backtrace as _;
use esp_hal::{delay::Delay, main};
use log::*;
use t5s3_epaper_core::{pin_config, Display, DrawMode};

esp_bootloader_esp_idf::esp_app_desc!();

#[main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();

    let peripherals = esp_hal::init(esp_hal::Config::default());
    let delay = Delay::new();

    info!("Create PSRAM allocator");
    esp_alloc::psram_allocator!(peripherals.PSRAM, esp_hal::psram);

    info!("Initialise the display");
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

    info!("Turn the display on");
    display.power_on().expect("to power on display");
    delay.delay_millis(10);

    info!("clear the screen");
    display.clear().expect("to clear screen");

    info!("Draw a circle with a 3px wide stroke in the center of the screen");
    Circle::new(display.bounding_box().center() - Point::new(100, 100), 200)
        .into_styled(PrimitiveStyle::with_stroke(Gray4::BLACK, 3))
        .draw(&mut display)
        .expect("to draw in the framebuffer");
    info!("Flush the framebuffer to the screen");
    display
        .flush(DrawMode::BlackOnWhite)
        .expect("to flush to display");

    info!("Turn the display off again");
    display.power_off().expect("to power off display");

    info!("do nothing");
    loop {
        core::hint::spin_loop();
    }
}
