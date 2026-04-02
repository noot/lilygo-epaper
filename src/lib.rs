//! Simple driver for the LilyGo T5 4.7 inch E-Ink Display.
//! The driver is wired for the LilyGo T5 S3 Paper Pro Lite / T5S3 4.7 inch
//! E-Paper Pro hardware variant (ESP32-S3).
//!
//! This library depends on alloc and requires you to set up an global allocator
//! for the PSRAM.
//!
//! `Display::flush()` is the general-purpose update path.
//! `Display::flush_partial_fast()` is available for low-flicker monochrome
//! updates on small rectangular UI regions.
//! `power::wake_status()`, `Display::deep_sleep()`, and `power::shutdown()`
//! expose the board's reset/wakeup and power-management paths.
//! `rtc::Clock` exposes the RTC-backed clock functions.
//!
//!
//! Built using [`esp-hal`] and [`embedded-graphics`]
//!
//! [`esp-hal`]: https://github.com/esp-rs/esp-hal
//! [`embedded-graphics`]: https://docs.rs/embedded-graphics/

//! # Example
//!
//! Simple example that draws a circle to the screen
//!
//! ```rust no_run
//! #![no_std]
//! #![no_main]
//! extern crate alloc;
//!
//! use embedded_graphics::{
//!     prelude::*,
//!     primitives::{Circle, PrimitiveStyle},
//! };
//! use embedded_graphics_core::pixelcolor::{Gray4, GrayColor};
//! use esp_backtrace as _;
//! use esp_hal::{
//!     delay::Delay,
//!     prelude::*,
//! };
//! use lilygo_epd47::{pin_config, Display, DrawMode};
//!
//! #[entry]
//! fn main() -> ! {
//!     let peripherals = esp_hal::init(esp_hal::Config::default());
//!     let delay = Delay::new();
//!     // Create PSRAM allocator
//!     esp_alloc::psram_allocator!(peripherals.PSRAM, esp_hal::psram);
//!     // Initialise the display
//!     let mut display = Display::new(
//!         pin_config!(peripherals),
//!         peripherals.I2C0,
//!         peripherals.DMA_CH0,
//!         peripherals.LCD_CAM,
//!         peripherals.RMT,
//!     )
//!     .expect("to initialize display");
//!     // Turn the display on
//!     display.power_on().unwrap();
//!     delay.delay_millis(10);
//!     // clear the screen
//!     display.clear().unwrap();
//!     // Draw a circle with a 3px wide stroke in the center of the screen
//!     // TODO: Adapt to your requirements (i.e. draw whatever you want)
//!     Circle::new(display.bounding_box().center() - Point::new(100, 100), 200)
//!         .into_styled(PrimitiveStyle::with_stroke(Gray4::BLACK, 3))
//!         .draw(&mut display)
//!         .unwrap();
//!     // Flush the framebuffer to the screen
//!     display.flush(DrawMode::BlackOnWhite).unwrap();
//!     // Turn the display of again
//!     display.power_off().unwrap();
//!     // do nothing
//!     loop {}
//! }
//! ```
//!
//! For small black-on-white UI regions, you can use the fast direct-update path:
//!
//! ```rust no_run
//! use lilygo_epd47::display::Rectangle;
//!
//! let area = Rectangle {
//!     x: 40,
//!     y: 200,
//!     width: 320,
//!     height: 80,
//! };
//!
//! display.flush_partial_fast(area).unwrap();
//! ```
#![no_std]

extern crate alloc;

pub mod display;
pub mod input;
pub mod power;
pub mod rtc;
pub mod touchscreen;

#[cfg(feature = "embedded-graphics")]
pub mod graphics;

mod battery;
mod ed047tc1;
mod rmt;

/// Errors
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Error {
    /// Pass-through
    Rmt(esp_hal::rmt::Error),
    /// Pass-through
    Dma(esp_hal::dma::DmaError),
    /// Pass-through
    DmaBuffer(esp_hal::dma::DmaBufError),
    /// Pass-through
    I2c(esp_hal::i2c::master::Error),
    /// Pass-through
    I2cConfig(esp_hal::i2c::master::ConfigError),
    /// Provided pixel coordinates exceed the display boundary.
    OutOfBounds,
    /// Provided color exceeds the allowed range of 0x0 - 0x0F
    InvalidColor,
    /// Timed out waiting for the power supply to report ready.
    PowerTimeout,
    /// LCD peripheral handle was unexpectedly unavailable.
    MissingI8080,
    /// DMA buffer was unexpectedly unavailable.
    MissingDmaBuffer,
    /// RMT output pin was unexpectedly unavailable.
    MissingRmtPin,
    /// RMT channel was unexpectedly unavailable.
    MissingRmtChannel,
    /// Touch controller initialization failed.
    TouchInitFailed,
}

type Result<T> = core::result::Result<T, Error>;

pub use crate::{
    battery::Battery,
    display::{Display, DrawMode},
    ed047tc1::PinConfig,
    input::{Buttons, InputState},
    power::WakeStatus,
    rtc::Clock,
    touchscreen::{TouchPoint, TouchState},
};

/// Convenience macro to build the pin config struct.
#[macro_export]
macro_rules! pin_config {
    ($name:expr) => {{
        lilygo_epd47::PinConfig {
            data0: $name.GPIO5,
            data1: $name.GPIO6,
            data2: $name.GPIO7,
            data3: $name.GPIO15,
            data4: $name.GPIO16,
            data5: $name.GPIO17,
            data6: $name.GPIO18,
            data7: $name.GPIO8,
            i2c_sda: $name.GPIO39,
            i2c_scl: $name.GPIO40,
            leh: $name.GPIO42,
            lcd_dc: $name.GPIO41,
            lcd_wrx: $name.GPIO4,
            rmt: $name.GPIO48,
            stv: $name.GPIO45,
            touch_int: $name.GPIO3,
            touch_rst: $name.GPIO9,
            boot_btn: $name.GPIO0,
        }
    }};
}
