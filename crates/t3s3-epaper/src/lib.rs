//! Firmware support library for the LilyGO T3-S3 e-paper board (SX1262
//! variant).
//!
//! Provides board pin definitions and a blocking driver for the Semtech SX1262
//! LoRa radio. The driver is generic over `embedded-hal` 1.0 traits so it does
//! not depend on any particular MCU HAL.

#![no_std]

pub mod board;
pub mod ssd1680;
pub mod sx1262;
