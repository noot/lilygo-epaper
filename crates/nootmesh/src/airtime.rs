//! LoRa packet time-on-air (SX126x datasheet section 6.1.4).
//!
//! Airtime is fully determined by the modulation parameters and payload
//! length, which is what lets a TDMA receiver recover the sender's slot start
//! from an RxDone timestamp: `slot_start = rx_done - airtime - guard`.

/// Modulation parameters needed to compute packet airtime.
///
/// Packets are assumed to use an explicit header and payload CRC, matching the
/// t3s3 sx1262 driver's packet configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Modulation {
    spreading_factor: u8,
    bandwidth_hz: u32,
    coding_rate_denominator: u8,
    preamble_symbols: u16,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("spreading factor {0} outside 7..=12")]
    SpreadingFactor(u8),
    #[error("bandwidth must be nonzero")]
    Bandwidth,
    #[error("coding rate denominator {0} outside 5..=8 (4/5 through 4/8)")]
    CodingRate(u8),
}

impl Modulation {
    pub fn new(
        spreading_factor: u8,
        bandwidth_hz: u32,
        coding_rate_denominator: u8,
        preamble_symbols: u16,
    ) -> Result<Self, Error> {
        if !(7..=12).contains(&spreading_factor) {
            return Err(Error::SpreadingFactor(spreading_factor));
        }
        if bandwidth_hz == 0 {
            return Err(Error::Bandwidth);
        }
        if !(5..=8).contains(&coding_rate_denominator) {
            return Err(Error::CodingRate(coding_rate_denominator));
        }
        Ok(Self {
            spreading_factor,
            bandwidth_hz,
            coding_rate_denominator,
            preamble_symbols,
        })
    }

    fn symbol_us(&self) -> u64 {
        ((1u64 << self.spreading_factor) * 1_000_000) / u64::from(self.bandwidth_hz)
    }

    /// Time on air in microseconds for a packet with `payload_len` bytes.
    pub fn packet_airtime_us(&self, payload_len: u8) -> u64 {
        let sf = u64::from(self.spreading_factor);
        // low-data-rate optimization is mandatory once a symbol exceeds
        // 16.38 ms (SF11/SF12 at 125 kHz) and steals 2 bits per symbol
        let de = u64::from(self.symbol_us() >= 16_380);
        // 8*PL - 4*SF + 28 + 16 (explicit header, CRC on), clamped at zero
        let numerator = (8 * u64::from(payload_len) + 44).saturating_sub(4 * sf);
        let payload_symbols = if numerator > 0 {
            let blocks = numerator.div_ceil(4 * (sf - 2 * de));
            8 + blocks * u64::from(self.coding_rate_denominator)
        } else {
            8
        };
        // the preamble adds 4.25 symbols; count quarter-symbols to stay integral
        let quarter_symbols = 4 * u64::from(self.preamble_symbols) + 17 + 4 * payload_symbols;
        quarter_symbols * self.symbol_us() / 4
    }
}

impl Modulation {
    /// The demodulation SNR limit for this spreading factor (LoRa decodes
    /// below the noise floor; the limit deepens ~2.5 dB per SF step). Below
    /// this, packets are gone. Rounded toward zero from the datasheet's
    /// half-dB values.
    pub fn snr_floor_db(&self) -> i16 {
        (100 - 25 * i16::from(self.spreading_factor)) / 10
    }

    /// The receive sensitivity floor in dBm: thermal noise in this bandwidth
    /// plus a typical SX126x noise figure (6 dB), plus the SNR limit.
    /// Signals below this cannot be received at all; expect heavy loss
    /// within ~10 dB above it (fading swings that much while walking).
    pub fn sensitivity_floor_dbm(&self) -> i16 {
        let bw_db = match self.bandwidth_hz {
            250_000 => 54,
            500_000 => 57,
            _ => 51,
        };
        -174 + bw_db + 6 + self.snr_floor_db()
    }

    pub fn spreading_factor(&self) -> u8 {
        self.spreading_factor
    }

    pub fn bandwidth_hz(&self) -> u32 {
        self.bandwidth_hz
    }

    pub fn coding_rate_denominator(&self) -> u8 {
        self.coding_rate_denominator
    }

    pub fn preamble_symbols(&self) -> u16 {
        self.preamble_symbols
    }
}

impl Default for Modulation {
    /// The fleet profile every node must share: SF9, 125 kHz, CR 4/5,
    /// 8-symbol preamble (~+6 dB link budget over SF7, roughly doubling
    /// range, at 4x the airtime — see the modulation ladder in nootmesh.md).
    /// Radio drivers derive their configuration from these accessors rather
    /// than hardcoding, so changing the profile here changes every firmware
    /// target together — with `tdma::Config::default()`, which must be
    /// resized to match.
    fn default() -> Self {
        Self {
            spreading_factor: 9,
            bandwidth_hz: 125_000,
            coding_rate_denominator: 5,
            preamble_symbols: 8,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sf7_reference_values() {
        let m = Modulation::new(7, 125_000, 5, 8).unwrap();
        assert_eq!(m.packet_airtime_us(12), 41_216);
        assert_eq!(m.packet_airtime_us(64), 118_016);
        assert_eq!(m.packet_airtime_us(255), 399_616);
    }

    #[test]
    fn sf9_profile_reference_values() {
        let m = Modulation::default();
        assert_eq!(m.packet_airtime_us(16), 164_864);
        assert_eq!(m.packet_airtime_us(71), 410_624);
        assert_eq!(m.snr_floor_db(), -12);
        assert_eq!(m.sensitivity_floor_dbm(), -129);
    }

    #[test]
    fn link_floors_deepen_with_sf() {
        let sf7 = Modulation::new(7, 125_000, 5, 8).unwrap();
        assert_eq!(sf7.snr_floor_db(), -7);
        assert_eq!(sf7.sensitivity_floor_dbm(), -124);
        let sf12 = Modulation::new(12, 125_000, 5, 8).unwrap();
        assert_eq!(sf12.snr_floor_db(), -20);
        assert_eq!(sf12.sensitivity_floor_dbm(), -137);
    }

    #[test]
    fn sf12_uses_low_data_rate_optimize() {
        let m = Modulation::new(12, 125_000, 5, 8).unwrap();
        assert_eq!(m.packet_airtime_us(16), 1_318_912);
    }

    #[test]
    fn rejects_invalid_params() {
        assert_eq!(
            Modulation::new(6, 125_000, 5, 8),
            Err(Error::SpreadingFactor(6))
        );
        assert_eq!(Modulation::new(7, 0, 5, 8), Err(Error::Bandwidth));
        assert_eq!(Modulation::new(7, 125_000, 9, 8), Err(Error::CodingRate(9)));
    }
}
