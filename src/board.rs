//! Pin assignments and defaults for the LilyGO T3-S3 e-paper board (SX1262).
//!
//! GPIO numbers cross-checked against LilyGO's factory `utilities.h` and the
//! Meshtastic `tlora_t3s3_epaper` variant. The LoRa radio sits on its own SPI
//! bus; the e-paper display and SD card share a second bus.

// lora radio (sx1262) — dedicated spi bus
pub const LORA_SCK: u8 = 5;
pub const LORA_MOSI: u8 = 6;
pub const LORA_MISO: u8 = 3;
pub const LORA_NSS: u8 = 7;
pub const LORA_RST: u8 = 8;
pub const LORA_BUSY: u8 = 34;
pub const LORA_DIO1: u8 = 33;
// radio power-enable: the factory firmware drives this high in initBoard() to
// power the radio's oscillator/analog rail. without it the xosc never starts
// (GetDeviceErrors reports XOSC_START_ERR) and every tx/rx times out.
pub const LORA_POW: u8 = 35;

// e-paper display (separate spi bus) — reserved for future use
pub const EINK_MOSI: u8 = 11;
pub const EINK_SCLK: u8 = 14;
pub const EINK_CS: u8 = 15;
pub const EINK_DC: u8 = 16;
pub const EINK_RST: u8 = 47;
pub const EINK_BUSY: u8 = 48;

// board controls
pub const LED: u8 = 37;
pub const BUTTON: u8 = 0;
pub const BATTERY_ADC: u8 = 1;

/// Default LoRa centre frequency for this board (US/AU 915 MHz).
pub const DEFAULT_FREQUENCY_HZ: u32 = 915_000_000;
