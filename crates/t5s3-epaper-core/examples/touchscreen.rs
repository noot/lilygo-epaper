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
use t5s3_epaper_core::{display::Rectangle, pin_config, Display};

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

    let delay = Delay::new();
    display.power_on().expect("to power on display");
    delay.delay_millis(10);
    display.clear().expect("to clear display");
    esp_println::println!("display bounds {:?}", display.bounding_box().size);

    loop {
        while let Some(event) = t5s3_epaper_core::i2c::poll_event() {
            match event {
                t5s3_epaper_core::i2c::Event::Home => {
                    esp_println::println!("home button pressed");
                }
                t5s3_epaper_core::i2c::Event::Tap { x, y } => {
                    esp_println::println!("touch x={} y={}", x, y);
                    let radius = 12i32;
                    let center = Point::new(x as i32, y as i32);
                    let top_left = center - Point::new(radius, radius);

                    Circle::new(top_left, (radius * 2) as u32)
                        .into_styled(PrimitiveStyle::with_fill(Gray4::BLACK))
                        .draw(&mut display)
                        .expect("to draw touch indicator");

                    let area = Rectangle {
                        x: x.saturating_sub(radius as u16 + 2),
                        y: y.saturating_sub(radius as u16 + 2),
                        width: (((radius as u16) * 2 + 4).min(Display::WIDTH)).min(
                            Display::WIDTH.saturating_sub(x.saturating_sub(radius as u16 + 2)),
                        ),
                        height: (((radius as u16) * 2 + 4).min(Display::HEIGHT)).min(
                            Display::HEIGHT.saturating_sub(y.saturating_sub(radius as u16 + 2)),
                        ),
                    };
                    display
                        .flush_partial_fast(area)
                        .expect("to flush touch indicator");
                }
            }
        }

        delay.delay_millis(100);
    }
}
