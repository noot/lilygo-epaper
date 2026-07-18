//! GPS-rooted TDMA on a shared LoRa channel.
//!
//! Time is divided into frames of [`Config::slots_per_frame`] slots. One
//! elected root (GPS-fixed nodes outrank the rest, then lowest id) anchors the
//! frame origin to UTC and floods it outward in [`Beacon`]s relayed by
//! stratum; see [`Sync`]. Data slots are then claimed collision-free via
//! distributed 2-hop graph coloring; see [`Coloring`].
//!
//! Frame layout: beacon slots first (the root transmits in slot 0, stratum-k
//! relays in slot `min(k, beacon_slots - 1)`), then contention slots where
//! nodes without a data slot send [`Hello`]s, then data slots. In-slot
//! transmissions start `guard_us` after the slot boundary so receivers with
//! slightly offset clocks still capture the whole packet.

pub mod engine;
mod slots;
mod sync;

pub use engine::{Action, Engine, PeerPosition, Received};
pub use slots::{Coloring, Hello, MAX_NEIGHBORS, PEER_ROWS, PeerInfo};
pub use sync::{Beacon, Sync};

const MAX_SLOTS_PER_FRAME: u16 = 256;

/// splitmix64 finalizer: a stable per-frame coin, so repeated decisions
/// within one frame agree with each other and both ends of a link can
/// reconstruct the same value.
pub(crate) fn mix(seed: u64, frame_number: u64, salt: u64) -> u64 {
    let mut z = seed
        ^ frame_number.wrapping_mul(0x9e37_79b9_7f4a_7c15)
        ^ salt.wrapping_mul(0xd1b5_4a32_d192_ed03);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// TDMA frame layout and timing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    slot_us: u64,
    slots_per_frame: u16,
    guard_us: u64,
    beacon_slots: u16,
    contention_slots: u16,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("slot ({slot_us} us) must be longer than twice the guard ({guard_us} us)")]
    SlotTooShort { slot_us: u64, guard_us: u64 },
    #[error("frame ({frame_us} us) must be a whole number of seconds to anchor to GPS time")]
    FrameNotWholeSeconds { frame_us: u64 },
    #[error("need at least one beacon slot")]
    NoBeaconSlots,
    #[error(
        "beacon ({beacon}) + contention ({contention}) slots leave no data slots in a {slots_per_frame}-slot frame"
    )]
    NoDataSlots {
        beacon: u16,
        contention: u16,
        slots_per_frame: u16,
    },
    #[error("{0} slots per frame exceeds the maximum of {MAX_SLOTS_PER_FRAME}")]
    TooManySlots(u16),
}

/// Which purpose a slot index serves within the frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotKind {
    Beacon,
    Contention,
    Data,
}

/// Where a local clock reading falls on the synchronized timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FramePosition {
    pub frame_number: u64,
    pub slot: u16,
    /// Microseconds elapsed since the start of `slot`.
    pub offset_us: u64,
}

impl Config {
    pub fn new(
        slot_us: u64,
        slots_per_frame: u16,
        guard_us: u64,
        beacon_slots: u16,
        contention_slots: u16,
    ) -> Result<Self, Error> {
        if slot_us <= 2 * guard_us {
            return Err(Error::SlotTooShort { slot_us, guard_us });
        }
        if slots_per_frame > MAX_SLOTS_PER_FRAME {
            return Err(Error::TooManySlots(slots_per_frame));
        }
        if beacon_slots == 0 {
            return Err(Error::NoBeaconSlots);
        }
        if beacon_slots + contention_slots >= slots_per_frame {
            return Err(Error::NoDataSlots {
                beacon: beacon_slots,
                contention: contention_slots,
                slots_per_frame,
            });
        }
        let config = Self {
            slot_us,
            slots_per_frame,
            guard_us,
            beacon_slots,
            contention_slots,
        };
        if !config.frame_us().is_multiple_of(1_000_000) {
            return Err(Error::FrameNotWholeSeconds {
                frame_us: config.frame_us(),
            });
        }
        Ok(config)
    }

    pub fn slot_us(&self) -> u64 {
        self.slot_us
    }

    pub fn slots_per_frame(&self) -> u16 {
        self.slots_per_frame
    }

    /// Delay after a slot boundary before in-slot transmission starts.
    pub fn guard_us(&self) -> u64 {
        self.guard_us
    }

    pub fn frame_us(&self) -> u64 {
        self.slot_us * u64::from(self.slots_per_frame)
    }

    fn frame_seconds(&self) -> u64 {
        self.frame_us() / 1_000_000
    }

    fn beacon_slots(&self) -> u16 {
        self.beacon_slots
    }

    /// Index of the first data slot; data slots run from here to the end of
    /// the frame.
    pub fn first_data_slot(&self) -> u16 {
        self.beacon_slots + self.contention_slots
    }

    pub fn slot_kind(&self, slot: u16) -> SlotKind {
        if slot < self.beacon_slots {
            SlotKind::Beacon
        } else if slot < self.first_data_slot() {
            SlotKind::Contention
        } else {
            SlotKind::Data
        }
    }
}

impl Default for Config {
    /// The fleet profile every node must share (a mismatched frame layout
    /// cannot sync), sized to [`Modulation::default`]'s SF9/125 kHz: 750 ms
    /// slots fit a 138-byte payload (720 ms) inside 15 ms guards, and 12
    /// slots make a 9-second frame (whole seconds, anchoring at
    /// `utc % 9 == 0`). 3 beacon + 3 contention slots leave 6 data slots.
    /// Slots are sized so the 41-byte encrypted beacon (288 ms) fits half
    /// the budget, leaving the other half as transmit jitter range for root
    /// decorrelation (see `beacon_tx_jitter_us`). See the modulation ladder
    /// in nootmesh.md for the slot/frame sizing at other spreading factors.
    ///
    /// [`Modulation::default`]: crate::airtime::Modulation::default
    fn default() -> Self {
        Self {
            slot_us: 750_000,
            slots_per_frame: 12,
            guard_us: 15_000,
            beacon_slots: 3,
            contention_slots: 3,
        }
    }
}

/// A fixed 16-second, 64-slot layout for unit tests, so their arithmetic is
/// independent of whatever the deployed profile in [`Config::default`]
/// currently is.
#[cfg(test)]
pub(crate) fn test_config() -> Config {
    match Config::new(250_000, 64, 15_000, 4, 6) {
        Ok(config) => config,
        Err(_) => unreachable!("test layout is valid"),
    }
}

/// The fixed SF7 modulation matching [`test_config`]'s airtime arithmetic,
/// independent of the deployed [`Modulation`] profile.
#[cfg(test)]
pub(crate) fn test_modulation() -> crate::airtime::Modulation {
    match crate::airtime::Modulation::new(7, 125_000, 5, 8) {
        Ok(modulation) => modulation,
        Err(_) => unreachable!("test modulation is valid"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_profile_is_9s_12_slots() {
        let config = Config::default();
        assert_eq!(config, Config::new(750_000, 12, 15_000, 3, 3).unwrap());
        assert_eq!(config.frame_us(), 9_000_000);
        assert_eq!(config.frame_seconds(), 9);
        assert_eq!(config.first_data_slot(), 6);
        // the profile pair stays workable: the sf9 modulation's packets fit
        // the slot budget with beacon jitter headroom
        let engine = crate::tdma::Engine::new(
            config,
            crate::airtime::Modulation::default(),
            crate::NodeId(1),
            7,
            &[7; 32],
        );
        assert!(engine.is_ok());
    }

    #[test]
    fn test_layout_is_16s_64_slots() {
        let config = test_config();
        assert_eq!(config.frame_us(), 16_000_000);
        assert_eq!(config.frame_seconds(), 16);
        assert_eq!(config.first_data_slot(), 10);
    }

    #[test]
    fn slot_kinds() {
        let config = Config::default();
        assert_eq!(config.slot_kind(0), SlotKind::Beacon);
        assert_eq!(config.slot_kind(2), SlotKind::Beacon);
        assert_eq!(config.slot_kind(3), SlotKind::Contention);
        assert_eq!(config.slot_kind(5), SlotKind::Contention);
        assert_eq!(config.slot_kind(6), SlotKind::Data);
        assert_eq!(config.slot_kind(11), SlotKind::Data);
    }

    #[test]
    fn rejects_invalid_layouts() {
        assert_eq!(
            Config::new(30_000, 100, 15_000, 4, 6),
            Err(Error::SlotTooShort {
                slot_us: 30_000,
                guard_us: 15_000,
            })
        );
        assert_eq!(
            Config::new(165_000, 100, 15_000, 4, 6),
            Err(Error::FrameNotWholeSeconds {
                frame_us: 16_500_000,
            })
        );
        assert_eq!(
            Config::new(160_000, 100, 15_000, 0, 6),
            Err(Error::NoBeaconSlots)
        );
        assert_eq!(
            Config::new(160_000, 10, 15_000, 4, 6),
            Err(Error::NoDataSlots {
                beacon: 4,
                contention: 6,
                slots_per_frame: 10,
            })
        );
        assert_eq!(
            Config::new(160_000, 500, 15_000, 4, 6),
            Err(Error::TooManySlots(500))
        );
    }
}
