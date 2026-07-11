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

impl Default for Modulation {
    /// The t3s3 sx1262 driver defaults: SF7, 125 kHz, CR 4/5, 8-symbol preamble.
    fn default() -> Self {
        Self {
            spreading_factor: 7,
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
        let m = Modulation::default();
        assert_eq!(m.packet_airtime_us(12), 41_216);
        assert_eq!(m.packet_airtime_us(64), 118_016);
        assert_eq!(m.packet_airtime_us(255), 399_616);
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
