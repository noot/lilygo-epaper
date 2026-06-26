//! Radio configuration types for the SX1262 LoRa modem.

/// LoRa spreading factor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpreadingFactor {
    Sf7,
    Sf8,
    Sf9,
    Sf10,
    Sf11,
    Sf12,
}

impl SpreadingFactor {
    pub(crate) fn reg(self) -> u8 {
        match self {
            Self::Sf7 => 0x07,
            Self::Sf8 => 0x08,
            Self::Sf9 => 0x09,
            Self::Sf10 => 0x0A,
            Self::Sf11 => 0x0B,
            Self::Sf12 => 0x0C,
        }
    }
}

/// LoRa signal bandwidth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bandwidth {
    Bw125kHz,
    Bw250kHz,
    Bw500kHz,
}

impl Bandwidth {
    pub(crate) fn reg(self) -> u8 {
        match self {
            Self::Bw125kHz => 0x04,
            Self::Bw250kHz => 0x05,
            Self::Bw500kHz => 0x06,
        }
    }

    pub(crate) fn hz(self) -> u32 {
        match self {
            Self::Bw125kHz => 125_000,
            Self::Bw250kHz => 250_000,
            Self::Bw500kHz => 500_000,
        }
    }
}

/// LoRa forward-error-correction coding rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodingRate {
    Cr4_5,
    Cr4_6,
    Cr4_7,
    Cr4_8,
}

impl CodingRate {
    pub(crate) fn reg(self) -> u8 {
        match self {
            Self::Cr4_5 => 0x01,
            Self::Cr4_6 => 0x02,
            Self::Cr4_7 => 0x03,
            Self::Cr4_8 => 0x04,
        }
    }
}

/// RadioLib's "private network" LoRa sync word, the default for a stock RadioLib
/// peer. Both endpoints must agree on the sync word to communicate.
pub const SYNC_WORD_PRIVATE: u16 = 0x1424;

/// Radio configuration applied during [`crate::sx1262::Sx1262::init`].
#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// Centre frequency in Hz.
    pub frequency_hz: u32,
    pub bandwidth: Bandwidth,
    pub spreading_factor: SpreadingFactor,
    pub coding_rate: CodingRate,
    /// Preamble length in symbols.
    pub preamble_len: u16,
    /// Transmit power in dBm (-9..=22 for the SX1262).
    pub tx_power_dbm: i8,
    /// LoRa sync word (both endpoints must match).
    pub sync_word: u16,
}

impl Default for Config {
    /// Sensible defaults for the T3-S3 e-paper board: 915 MHz, SF7, 125 kHz,
    /// 4/5, +22 dBm, private sync word.
    fn default() -> Self {
        Self {
            frequency_hz: crate::board::DEFAULT_FREQUENCY_HZ,
            bandwidth: Bandwidth::Bw125kHz,
            spreading_factor: SpreadingFactor::Sf7,
            coding_rate: CodingRate::Cr4_5,
            preamble_len: 8,
            tx_power_dbm: 22,
            sync_word: SYNC_WORD_PRIVATE,
        }
    }
}

impl Config {
    /// Convert the centre frequency to the 32-bit PLL value expected by
    /// `SetRfFrequency`: `freq * 2^25 / 32_000_000`.
    pub(crate) fn frequency_pll(&self) -> u32 {
        const XTAL_HZ: u64 = 32_000_000;
        (((self.frequency_hz as u64) << 25) / XTAL_HZ) as u32
    }

    /// Image-calibration band bytes for the configured frequency (datasheet
    /// `CalibrateImage` table).
    pub(crate) fn calibrate_image_band(&self) -> [u8; 2] {
        match self.frequency_hz {
            f if (902_000_000..=928_000_000).contains(&f) => [0xE1, 0xE9],
            f if (863_000_000..=870_000_000).contains(&f) => [0xD7, 0xDB],
            f if (779_000_000..=787_000_000).contains(&f) => [0xC1, 0xC5],
            f if (470_000_000..=510_000_000).contains(&f) => [0x75, 0x81],
            // default to the 430-440 MHz band
            _ => [0x6B, 0x6F],
        }
    }

    /// Low-data-rate optimization is required when symbol duration exceeds
    /// 16.38 ms (datasheet 6.1.4).
    pub(crate) fn low_data_rate_optimize(&self) -> u8 {
        let symbol_us =
            ((1u64 << self.spreading_factor.reg()) * 1_000_000) / self.bandwidth.hz() as u64;
        u8::from(symbol_us >= 16_380)
    }
}
