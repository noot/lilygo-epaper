#![no_std]
#![no_main]

extern crate alloc;
extern crate lilygo_epd47;

use embedded_graphics::{
    prelude::*,
    primitives::{Circle, PrimitiveStyle},
};
use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
use esp_backtrace as _;
use esp_hal::{delay::Delay, main};
use lilygo_epd47::display::Rectangle;
use lilygo_epd47::{pin_config, Display};

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
    display.clear().expect("to clear display");
    esp_println::println!(
        "Touch resolution {:?}, display bounds {:?}",
        display.touch_resolution(),
        display.bounding_box().size
    );

    loop {
        let input = display.input().expect("to read input state");
        if input.buttons.home {
            esp_println::println!("home button pressed");
        }
        if let Some(state) = input.touch {
            if let Some(point) = state.first_point() {
                esp_println::println!("touch x={} y={} size={}", point.x, point.y, point.size);
                let radius = 12i32;
                let center = Point::new(point.x as i32, point.y as i32);
                let top_left = center - Point::new(radius, radius);

                Circle::new(top_left, (radius * 2) as u32)
                    .into_styled(PrimitiveStyle::with_fill(Gray4::BLACK))
                    .draw(&mut display)
                    .expect("to draw touch indicator");

                let area = Rectangle {
                    x: point.x.saturating_sub(radius as u16 + 2),
                    y: point.y.saturating_sub(radius as u16 + 2),
                    width: (((radius as u16) * 2 + 4).min(Display::WIDTH))
                        .min(Display::WIDTH.saturating_sub(point.x.saturating_sub(radius as u16 + 2))),
                    height: (((radius as u16) * 2 + 4).min(Display::HEIGHT))
                        .min(Display::HEIGHT.saturating_sub(point.y.saturating_sub(radius as u16 + 2))),
                };
                display
                    .flush_partial_fast(area)
                    .expect("to flush touch indicator");
            }
        }

        delay.delay_millis(100);
    }
}
