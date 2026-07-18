use super::{Config, FramePosition, mix};
use crate::NodeId;

const SALT_BEACON_TIME: u64 = 6;

/// Deterministic per-frame offset added to a beacon's in-slot transmit time
/// (on top of the guard). Derived from the root id and frame number carried
/// in the beacon itself, so receivers reconstruct it exactly when recovering
/// the frame origin.
///
/// Without it, two contending roots anchored to the same timeline — two
/// GPS-fixed nodes both anchor to the UTC grid by construction — transmit
/// their slot-0 beacons at the same instant every frame, collide forever, and
/// never hear each other to resolve the election. The jitter decorrelates
/// them frame by frame, so the losing root hears the winner within a few
/// frames. (Same-root relays share this value; the engine's random skip
/// decorrelates those.)
pub(crate) fn beacon_tx_jitter_us(config: &Config, root: NodeId, frame_number: u64) -> u64 {
    let range = (config.slot_us() - 2 * config.guard_us()) / 2;
    if range == 0 {
        return 0;
    }
    mix(u64::from(root.0), frame_number, SALT_BEACON_TIME) % range
}

/// Frames of holdover before a beacon-synced node considers itself unsynced.
/// At +/-20 ppm crystal drift, 8 default frames (128 s) accrue ~2.6 ms of
/// error, well inside the guard. The root itself never expires: it is its own
/// time source and cedes only to an outranking beacon.
const EXPIRY_FRAMES: u64 = 8;

/// Beacons at this stratum or above are not adopted (and not sent), bounding
/// accumulated per-hop timestamp error to a few milliseconds.
const MAX_STRATUM: u8 = 7;

/// How far the root's local timeline may drift from GPS-reported UTC before it
/// re-anchors. NMEA-over-UART timing jitters by tens of milliseconds, so the
/// root must free-run on its crystal and ignore that jitter; only a gross
/// disagreement (clock glitch, first fix after long holdover) forces a
/// re-anchor, which steps the whole mesh's timeline.
const REANCHOR_THRESHOLD_US: u64 = 200_000;

/// Time beacon flooded outward from the elected root each frame.
///
/// A receiver recovers the frame origin from its RxDone timestamp alone: the
/// sender's beacon slot is `min(stratum, beacon_slots - 1)` and the airtime is
/// computable, so no timestamp field is needed on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Beacon {
    pub root: NodeId,
    pub root_has_gps: bool,
    pub stratum: u8,
    pub frame_number: u64,
}

#[derive(Debug, Clone, Copy)]
struct Synced {
    root: NodeId,
    root_has_gps: bool,
    stratum: u8,
    frame_number: u64,
    /// Local clock at the start of frame `frame_number`.
    frame_origin_us: u64,
    synced_at_us: u64,
}

/// Time synchronization state machine: elects a root, tracks the frame
/// timeline, and decides which beacons to adopt and relay.
///
/// The caller drives it with local-clock events: `on_gps_second` per NMEA
/// sentence while holding a fix, `on_beacon` per received beacon, and reads
/// the timeline back via `position` and `beacon`.
pub struct Sync {
    config: Config,
    node_id: NodeId,
    state: Option<Synced>,
}

impl Sync {
    pub fn new(config: Config, node_id: NodeId) -> Self {
        Self {
            config,
            node_id,
            state: None,
        }
    }

    fn effective(&self, now_us: u64) -> Option<&Synced> {
        self.state.as_ref().filter(|s| {
            s.root == self.node_id
                || now_us.saturating_sub(s.synced_at_us) <= EXPIRY_FRAMES * self.config.frame_us()
        })
    }

    /// Where `now_us` falls on the synchronized timeline, or `None` when
    /// unsynced (never synced, or sync expired without refresh).
    pub fn position(&self, now_us: u64) -> Option<FramePosition> {
        let state = self.effective(now_us)?;
        let elapsed = now_us.checked_sub(state.frame_origin_us)?;
        let frame_us = self.config.frame_us();
        Some(FramePosition {
            frame_number: state.frame_number + elapsed / frame_us,
            slot: ((elapsed % frame_us) / self.config.slot_us()) as u16,
            offset_us: elapsed % self.config.slot_us(),
        })
    }

    /// Feed a UTC second boundary from GPS. `now_us` is the local clock when
    /// the boundary was observed (NMEA arrival, or a PPS edge if wired).
    ///
    /// The node roots itself if it outranks the current root (GPS beats
    /// non-GPS, then lowest id). An established root refreshes its sync
    /// lifetime but keeps free-running on its own crystal, re-anchoring only
    /// past [`REANCHOR_THRESHOLD_US`] so NMEA jitter never steps the timeline.
    pub fn on_gps_second(&mut self, now_us: u64, utc_seconds: u64) {
        let already_root = self
            .effective(now_us)
            .is_some_and(|s| s.root == self.node_id);
        if already_root {
            // a free-running root that gains a fix upgrades to a gps-anchored
            // timeline (stepping it once)
            if self.state.as_ref().is_some_and(|s| !s.root_has_gps) {
                self.anchor(now_us, utc_seconds);
                return;
            }
            if let Some(position) = self.position(now_us) {
                let predicted_us = position.frame_number * self.config.frame_us()
                    + u64::from(position.slot) * self.config.slot_us()
                    + position.offset_us;
                if predicted_us.abs_diff(utc_seconds * 1_000_000) <= REANCHOR_THRESHOLD_US {
                    if let Some(state) = self.state.as_mut() {
                        state.synced_at_us = now_us;
                    }
                    return;
                }
            }
            self.anchor(now_us, utc_seconds);
            return;
        }
        let outranked = match self.effective(now_us) {
            None => true,
            Some(s) => outranks(true, self.node_id, s.root_has_gps, s.root),
        };
        if outranked {
            self.anchor(now_us, utc_seconds);
        }
    }

    fn anchor(&mut self, now_us: u64, utc_seconds: u64) {
        let into_frame_us = (utc_seconds % self.config.frame_seconds()) * 1_000_000;
        // just after boot the local clock may not reach back to the current
        // frame's start; skip until it does (at most one frame)
        let Some(frame_origin_us) = now_us.checked_sub(into_frame_us) else {
            return;
        };
        self.state = Some(Synced {
            root: self.node_id,
            root_has_gps: true,
            stratum: 0,
            frame_number: utc_seconds / self.config.frame_seconds(),
            frame_origin_us,
            synced_at_us: now_us,
        });
    }

    /// Self-appoint as a free-running (non-GPS) root, anchoring the frame
    /// origin at `now_us`. No-op while synced: this is the fallback for a
    /// mesh with no GPS anywhere in reach, invoked after listening long
    /// enough to be confident no root exists. A GPS-anchored beacon heard
    /// later outranks and displaces this timeline.
    pub fn become_root(&mut self, now_us: u64) {
        if self.effective(now_us).is_some() {
            return;
        }
        self.state = Some(Synced {
            root: self.node_id,
            root_has_gps: false,
            stratum: 0,
            frame_number: 0,
            frame_origin_us: now_us,
            synced_at_us: now_us,
        });
    }

    /// Current root and this node's stratum, for status displays.
    pub fn root(&self, now_us: u64) -> Option<(NodeId, u8)> {
        self.effective(now_us).map(|s| (s.root, s.stratum))
    }

    /// UTC in whole seconds at `now_us`, when the timeline is GPS-anchored
    /// (only a GPS root's frame numbers are `utc / frame_seconds`; a
    /// free-running timeline has no UTC meaning).
    pub fn utc_seconds(&self, now_us: u64) -> Option<u64> {
        let state = self.effective(now_us)?;
        if !state.root_has_gps {
            return None;
        }
        let elapsed = now_us.checked_sub(state.frame_origin_us)?;
        let frame_us = self.config.frame_us();
        let frame_number = state.frame_number + elapsed / frame_us;
        Some(frame_number * self.config.frame_seconds() + (elapsed % frame_us) / 1_000_000)
    }

    /// Feed a received beacon. `rx_end_us` is the local clock at RxDone and
    /// `airtime_us` the packet's computed time on air.
    pub fn on_beacon(&mut self, rx_end_us: u64, airtime_us: u64, beacon: &Beacon) {
        if beacon.stratum >= MAX_STRATUM {
            return;
        }
        let adopt = match self.effective(rx_end_us) {
            None => true,
            Some(s) if s.root == beacon.root => beacon.stratum < s.stratum,
            Some(s) => outranks(beacon.root_has_gps, beacon.root, s.root_has_gps, s.root),
        };
        if !adopt {
            return;
        }
        // the sender transmitted at its beacon slot's start + guard + a
        // jitter reconstructible from the beacon's own fields, so the frame
        // origin is recoverable from RxDone alone
        let sender_slot = u16::from(beacon.stratum).min(self.config.beacon_slots() - 1);
        let offset_us = airtime_us
            + self.config.guard_us()
            + beacon_tx_jitter_us(&self.config, beacon.root, beacon.frame_number)
            + u64::from(sender_slot) * self.config.slot_us();
        let Some(frame_origin_us) = rx_end_us.checked_sub(offset_us) else {
            return;
        };
        self.state = Some(Synced {
            root: beacon.root,
            root_has_gps: beacon.root_has_gps,
            stratum: beacon.stratum + 1,
            frame_number: beacon.frame_number,
            frame_origin_us,
            synced_at_us: rx_end_us,
        });
    }

    /// The beacon this node should relay, and the slot to send it in, or
    /// `None` when unsynced or too deep to relay.
    ///
    /// Transmit at the returned slot's start + guard. Nodes at the same
    /// stratum share a relay slot, so callers should randomly skip frames to
    /// decorrelate; collisions only delay sync, never corrupt it.
    pub fn beacon(&self, now_us: u64) -> Option<(u16, Beacon)> {
        let state = self.effective(now_us)?;
        if state.stratum >= MAX_STRATUM {
            return None;
        }
        let position = self.position(now_us)?;
        let slot = u16::from(state.stratum).min(self.config.beacon_slots() - 1);
        Some((
            slot,
            Beacon {
                root: state.root,
                root_has_gps: state.root_has_gps,
                stratum: state.stratum,
                frame_number: position.frame_number,
            },
        ))
    }
}

fn outranks(a_has_gps: bool, a: NodeId, b_has_gps: bool, b: NodeId) -> bool {
    if a_has_gps != b_has_gps {
        a_has_gps
    } else {
        a < b
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tdma::test_config;

    const FRAME_US: u64 = 16_000_000;

    fn synced_via_gps(node_id: u32, now_us: u64, utc_seconds: u64) -> Sync {
        let mut sync = Sync::new(test_config(), NodeId(node_id));
        sync.on_gps_second(now_us, utc_seconds);
        sync
    }

    #[test]
    fn gps_anchor_sets_position() {
        let sync = synced_via_gps(1, 20_000_000, 16_005);
        let position = sync.position(20_000_000).unwrap();
        assert_eq!(position.frame_number, 1_000);
        assert_eq!(position.slot, 20);
        assert_eq!(position.offset_us, 0);

        let position = sync.position(20_000_000 + 11_000_000 + FRAME_US).unwrap();
        assert_eq!(position.frame_number, 1_002);
        assert_eq!(position.slot, 0);
    }

    #[test]
    fn anchor_skipped_when_clock_too_young() {
        let sync = synced_via_gps(1, 1_000_000, 16_005);
        assert_eq!(sync.position(1_000_000), None);
    }

    #[test]
    fn beacon_adoption_recovers_origin() {
        let mut sync = Sync::new(test_config(), NodeId(2));
        let beacon = Beacon {
            root: NodeId(1),
            root_has_gps: true,
            stratum: 0,
            frame_number: 42,
        };
        // sender began at origin + guard; rx ends one airtime later
        let origin = 5_000_000;
        let jitter = beacon_tx_jitter_us(&test_config(), NodeId(1), 42);
        let rx_end = origin + 15_000 + jitter + 41_216;
        sync.on_beacon(rx_end, 41_216, &beacon);

        let position = sync.position(origin + 200_000).unwrap();
        assert_eq!(position.frame_number, 42);
        assert_eq!(position.slot, 0);
        assert_eq!(position.offset_us, 200_000);

        let (slot, relayed) = sync.beacon(origin + 200_000).unwrap();
        assert_eq!(slot, 1);
        assert_eq!(relayed.stratum, 1);
        assert_eq!(relayed.root, NodeId(1));
        assert_eq!(relayed.frame_number, 42);
    }

    #[test]
    fn root_ranking() {
        let mut sync = Sync::new(test_config(), NodeId(9));
        let base = Beacon {
            root: NodeId(5),
            root_has_gps: true,
            stratum: 0,
            frame_number: 1,
        };
        sync.on_beacon(1_000_000, 41_216, &base);

        let worse = Beacon {
            root: NodeId(1),
            root_has_gps: false,
            ..base
        };
        sync.on_beacon(2_000_000, 41_216, &worse);
        assert_eq!(sync.beacon(2_100_000).unwrap().1.root, NodeId(5));

        let better = Beacon {
            root: NodeId(3),
            ..base
        };
        sync.on_beacon(3_000_000, 41_216, &better);
        assert_eq!(sync.beacon(3_100_000).unwrap().1.root, NodeId(3));
    }

    #[test]
    fn same_root_requires_lower_stratum() {
        let mut sync = Sync::new(test_config(), NodeId(9));
        let base = Beacon {
            root: NodeId(1),
            root_has_gps: true,
            stratum: 2,
            frame_number: 1,
        };
        sync.on_beacon(1_000_000, 41_216, &base);
        assert_eq!(sync.beacon(1_100_000).unwrap().1.stratum, 3);

        sync.on_beacon(2_000_000, 41_216, &Beacon { stratum: 3, ..base });
        assert_eq!(sync.beacon(2_100_000).unwrap().1.stratum, 3);

        sync.on_beacon(3_000_000, 41_216, &Beacon { stratum: 1, ..base });
        assert_eq!(sync.beacon(3_100_000).unwrap().1.stratum, 2);
    }

    #[test]
    fn deep_strata_not_adopted_or_relayed() {
        let mut sync = Sync::new(test_config(), NodeId(9));
        let beacon = Beacon {
            root: NodeId(1),
            root_has_gps: true,
            stratum: 7,
            frame_number: 1,
        };
        sync.on_beacon(1_000_000, 41_216, &beacon);
        assert_eq!(sync.position(1_100_000), None);

        sync.on_beacon(
            2_000_000,
            41_216,
            &Beacon {
                stratum: 6,
                ..beacon
            },
        );
        assert!(sync.position(2_100_000).is_some());
        assert_eq!(sync.beacon(2_100_000), None);
    }

    #[test]
    fn beacon_sync_expires_without_refresh() {
        let mut sync = Sync::new(test_config(), NodeId(9));
        let beacon = Beacon {
            root: NodeId(1),
            root_has_gps: true,
            stratum: 0,
            frame_number: 1,
        };
        sync.on_beacon(20_000_000, 41_216, &beacon);
        assert!(sync.position(20_000_000 + 8 * FRAME_US).is_some());
        assert_eq!(sync.position(20_000_000 + 8 * FRAME_US + 1), None);
    }

    #[test]
    fn root_never_expires() {
        let sync = synced_via_gps(1, 20_000_000, 16_000);
        assert!(sync.position(20_000_000 + 100 * FRAME_US).is_some());
    }

    #[test]
    fn free_running_root_fallback() {
        let mut sync = Sync::new(test_config(), NodeId(2));
        sync.become_root(50_000_000);
        let position = sync.position(50_000_000 + FRAME_US + 300_000).unwrap();
        assert_eq!(position.frame_number, 1);
        assert_eq!(position.slot, 1);
        let (slot, beacon) = sync.beacon(51_000_000).unwrap();
        assert_eq!(slot, 0);
        assert!(!beacon.root_has_gps);
        assert_eq!(beacon.root, NodeId(2));
        // no expiry: it is its own time source
        assert!(sync.position(50_000_000 + 100 * FRAME_US).is_some());

        // a gps-anchored root outranks it
        let better = Beacon {
            root: NodeId(9),
            root_has_gps: true,
            stratum: 0,
            frame_number: 500,
        };
        sync.on_beacon(60_000_000, 41_216, &better);
        assert_eq!(sync.root(60_100_000), Some((NodeId(9), 1)));
    }

    #[test]
    fn become_root_is_noop_while_synced() {
        let mut sync = synced_via_gps(1, 20_000_000, 16_000);
        sync.become_root(21_000_000);
        let (_, beacon) = sync.beacon(21_100_000).unwrap();
        assert!(beacon.root_has_gps);
        assert_eq!(beacon.frame_number, 1_000);
    }

    #[test]
    fn free_running_root_upgrades_on_gps_fix() {
        let mut sync = Sync::new(test_config(), NodeId(2));
        sync.become_root(50_000_000);
        sync.on_gps_second(60_000_000, 16_008);
        let (_, beacon) = sync.beacon(60_100_000).unwrap();
        assert!(beacon.root_has_gps);
        assert_eq!(beacon.frame_number, 1_000);
        assert_eq!(sync.position(60_100_000).unwrap().slot, 32);
    }

    #[test]
    fn root_refreshes_without_stepping_timeline() {
        let mut sync = synced_via_gps(1, 20_000_000, 16_000);
        // 10 ms of NMEA jitter must refresh the lifetime, not move the origin
        for i in 1..=20u64 {
            sync.on_gps_second(20_000_000 + i * 1_000_000 + 10_000, 16_000 + i);
        }
        let position = sync.position(20_000_000 + 20_000_000).unwrap();
        assert_eq!(position.frame_number, 1_001);
        assert_eq!(position.offset_us, 0);
        assert!(sync.position(40_000_000 + 8 * FRAME_US).is_some());
    }

    #[test]
    fn root_reanchors_on_gross_disagreement() {
        let mut sync = synced_via_gps(1, 20_000_000, 16_000);
        sync.on_gps_second(21_000_000, 16_003);
        let position = sync.position(21_000_000).unwrap();
        assert_eq!(position.slot, 12);
        assert_eq!(position.offset_us, 0);
    }

    #[test]
    fn gps_node_defers_to_better_root() {
        let mut sync = synced_via_gps(5, 20_000_000, 16_000);
        let beacon = Beacon {
            root: NodeId(3),
            root_has_gps: true,
            stratum: 0,
            frame_number: 1_000,
        };
        sync.on_beacon(21_000_000, 41_216, &beacon);
        assert_eq!(sync.beacon(21_100_000).unwrap().1.root, NodeId(3));
        // its own gps seconds no longer steal the root back
        sync.on_gps_second(22_000_000, 16_002);
        assert_eq!(sync.beacon(22_100_000).unwrap().1.root, NodeId(3));
    }
}
