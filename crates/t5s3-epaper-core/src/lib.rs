//! Rust driver fork for the LilyGo T5 S3 Paper Pro device family.
//!
//! This crate started from the original `fridolin-koch/lilygo-epd47-rs`
//! project and is now wired for the LilyGo T5 S3 Paper Pro Lite / T5S3 4.7
//! inch E-Paper Pro hardware variant (ESP32-S3), based heavily on analysis of
//! the official `Xinyuan-LilyGO/T5S3-4.7-e-paper-PRO` firmware.
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
//! `sdcard::SdCard` exposes the SPI-connected microSD slot.
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
//! use t5s3_epaper_core::{pin_config, Display, DrawMode};
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
//! For small black-on-white UI regions, you can use the fast direct-update
//! path:
//!
//! ```rust no_run
//! use t5s3_epaper_core::display::Rectangle;
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

mod bus;
pub mod display;
pub mod frontlight;
pub mod input;
pub mod power;
pub mod rtc;
pub mod sdcard;
pub mod touchscreen;

#[cfg(feature = "embedded-graphics")]
pub mod graphics;

#[cfg(feature = "gps")]
pub mod gps;

#[cfg(feature = "lora")]
pub mod lora;

mod battery;
mod ed047tc1;
mod rmt;

/// Errors
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Error {
    /// Pass-through
    Rmt(esp_hal::rmt::Error),
    /// Pass-through
    RmtConfig(esp_hal::rmt::ConfigError),
    /// Pass-through
    Dma(esp_hal::dma::DmaError),
    /// Pass-through
    DmaBuffer(esp_hal::dma::DmaBufError),
    /// Pass-through
    I2c(esp_hal::i2c::master::Error),
    /// Pass-through
    I2cConfig(esp_hal::i2c::master::ConfigError),
    /// I8080 LCD interface configuration failed.
    I8080(esp_hal::lcd_cam::lcd::i8080::ConfigError),
    /// ADC oneshot read failed.
    AdcRead,
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

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Rmt(e) => write!(f, "RMT error: {e:?}"),
            Self::RmtConfig(e) => write!(f, "RMT configuration error: {e:?}"),
            Self::Dma(e) => write!(f, "DMA error: {e:?}"),
            Self::DmaBuffer(e) => write!(f, "DMA buffer error: {e:?}"),
            Self::I2c(e) => write!(f, "I2C error: {e:?}"),
            Self::I2cConfig(e) => write!(f, "I2C configuration error: {e:?}"),
            Self::I8080(e) => write!(f, "I8080 LCD configuration error: {e:?}"),
            Self::AdcRead => write!(f, "ADC oneshot read failed"),
            Self::OutOfBounds => write!(f, "pixel coordinates exceed display boundary"),
            Self::InvalidColor => write!(f, "color exceeds allowed range of 0x0-0x0F"),
            Self::PowerTimeout => write!(f, "timed out waiting for power supply ready"),
            Self::MissingI8080 => write!(f, "LCD peripheral handle unexpectedly unavailable"),
            Self::MissingDmaBuffer => write!(f, "DMA buffer unexpectedly unavailable"),
            Self::MissingRmtPin => write!(f, "RMT output pin unexpectedly unavailable"),
            Self::MissingRmtChannel => write!(f, "RMT channel unexpectedly unavailable"),
            Self::TouchInitFailed => write!(f, "touch controller initialization failed"),
        }
    }
}

type Result<T> = core::result::Result<T, Error>;

pub use crate::{
    battery::Battery,
    display::{Display, DrawMode},
    ed047tc1::PinConfig,
    frontlight::FrontLight,
    input::{Buttons, InputState},
    power::WakeStatus,
    rtc::Clock,
    sdcard::{DirectoryEntry as SdDirectoryEntry, SdCard},
    touchscreen::{TouchPoint, TouchState},
};

/// Convenience macro to build the pin config struct.
#[macro_export]
macro_rules! pin_config {
    ($name:expr) => {{
        t5s3_epaper_core::PinConfig {
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

/// Convenience macro to build the GPS pin config struct.
#[cfg(feature = "gps")]
#[macro_export]
macro_rules! gps_pin_config {
    ($name:expr) => {{
        t5s3_epaper_core::gps::PinConfig {
            tx: $name.GPIO43,
            rx: $name.GPIO44,
        }
    }};
}

/// Convenience macro to build the LoRa pin config struct.
#[cfg(feature = "lora")]
#[macro_export]
macro_rules! lora_pin_config {
    ($name:expr) => {{
        t5s3_epaper_core::lora::PinConfig {
            cs: $name.GPIO46,
            rst: $name.GPIO1,
            busy: $name.GPIO47,
            dio1: $name.GPIO10,
        }
    }};
}
