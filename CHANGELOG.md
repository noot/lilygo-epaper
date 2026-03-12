# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased

### Changed

- `Battery::DEFAULT_CORRECTION_FACTOR` changed from `1.144632` to `1.0` since esp-hal 1.0's `AdcCalCurve` calibration is accurate enough without additional correction

### Fixed

- Fixed `Battery::read()` panic due to `read_oneshot` returning `nb::Error::WouldBlock` by wrapping with `nb::block!()`

### Added

- Added `nb` dependency

## 1.0.0 - 2026-03-11

### Changed

- Update esp-hal to `1.0.0`
- `Display::new` now takes concrete peripheral types (`DMA_CH0`, `LCD_CAM`, `RMT`) instead of `impl Peripheral<P = ...>`
- `Battery::new` now takes `ADC2<'a>` directly instead of `impl Peripheral<P = ADC2>`
- `pin_config!` macro now takes `peripherals` directly instead of a mux struct
- Update `esp-alloc` to `0.9.0`
- Update `esp-backtrace` to `0.18.1`
- Update `esp-println` to `0.16.1`
- Update `u8g2-fonts` to `0.7.2`

### Added

- Added `critical-section` dependency
- Added `esp-bootloader-esp-idf` dependency
- Added `log` as a direct dependency

### Removed

- Removed `esp-hal` `exception-handler` feature (no longer available)

## 0.5.0 - 2025-01-25

### Changed

- Update esp-hal to `0.22.0`

## 0.4.0 - 2025-01-23

### Changed

- Update esp-hal to `0.21.1`
- `Display::new` is now fallible and returns a `Result`

### Fixed

- Fixed multiple integer overflows

## 0.3.0 - 2024-09-26

### Changed

- Update esp-hal to `0.20.0`

## 0.2.0 - 2024-07-11

### Added

- Added `Battery` struct for reading battery voltage via ADC
- Added battery example

### Changed

- `pin_config!` macro now only takes required pins instead of full IO mux

## 0.1.1 - 2024-07-06

### Added

- Added deep sleep example
- Added demo image to documentation
