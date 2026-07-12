use crate::{NodeId, airtime::Modulation, wire};

use super::{Coloring, Config, FramePosition, Sync};

/// Frames to listen after first syncing before claiming a data slot, so the
/// neighbor table informs the first pick.
const LISTEN_FRAMES: u64 = 2;

/// Smallest packet capacity a slot must fit (beacons are 13 bytes).
const MIN_PACKET_LEN: u8 = 16;

const MAX_PACKET: usize = 255;

const SALT_RELAY: u64 = 1;
const SALT_CONTENTION_SEND: u64 = 2;
const SALT_CONTENTION_SLOT: u64 = 3;

/// What the radio should do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Nothing to transmit before `revisit_us`: stay in receive mode and call
    /// [`Engine::next_action`] again then, or after any event.
    Listen { revisit_us: u64 },
    /// Start transmitting [`Engine::packet`] at exactly `at_us` (a slot start
    /// plus the guard), receiving until then.
    Transmit { at_us: u64 },
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error(
        "slot budget of {budget_us} us cannot fit a {MIN_PACKET_LEN}-byte packet ({airtime_us} us)"
    )]
    SlotTooShort { budget_us: u64, airtime_us: u64 },
}

/// Ties [`Sync`], [`Coloring`] and the wire format into one scheduling loop.
///
/// The caller owns the radio and the clock, feeds events in (`on_packet`,
/// `on_gps_second`) and asks `next_action` what to do next. Policy implemented
/// here: the root beacons every frame while relays randomly skip half their
/// turns to decorrelate shared relay slots; a newly synced node listens for
/// [`LISTEN_FRAMES`] frames before claiming a data slot; a node with a slot
/// announces a [`wire::Message::Hello`] there every frame (keeping its claim
/// alive in neighbors' tables); a node that finds every slot taken falls back
/// to hellos in randomly chosen contention slots.
pub struct Engine {
    config: Config,
    modulation: Modulation,
    seed: u64,
    sync: Sync,
    coloring: Coloring,
    synced_since: Option<u64>,
    max_packet_len: u8,
    tx_buf: [u8; MAX_PACKET],
    tx_len: usize,
}

enum Payload {
    Beacon,
    Hello,
}

impl Engine {
    /// `seed` decorrelates random skips between nodes; give it boot entropy
    /// (it is additionally mixed with the node id, so equal seeds are safe).
    pub fn new(
        config: Config,
        modulation: Modulation,
        node_id: NodeId,
        seed: u64,
    ) -> Result<Self, Error> {
        let budget_us = config.slot_us() - 2 * config.guard_us();
        if modulation.packet_airtime_us(MIN_PACKET_LEN) > budget_us {
            return Err(Error::SlotTooShort {
                budget_us,
                airtime_us: modulation.packet_airtime_us(MIN_PACKET_LEN),
            });
        }
        let max_packet_len = (MIN_PACKET_LEN..=u8::MAX)
            .take_while(|len| modulation.packet_airtime_us(*len) <= budget_us)
            .last()
            .unwrap_or(MIN_PACKET_LEN);
        Ok(Self {
            config,
            modulation,
            seed: seed ^ u64::from(node_id.0),
            sync: Sync::new(config, node_id),
            coloring: Coloring::new(config, node_id),
            synced_since: None,
            max_packet_len,
            tx_buf: [0; MAX_PACKET],
            tx_len: 0,
        })
    }

    /// Feed a UTC second boundary from GPS while holding a fix.
    pub fn on_gps_second(&mut self, now_us: u64, utc_seconds: u64) {
        self.sync.on_gps_second(now_us, utc_seconds);
    }

    /// Feed a received packet. `rx_end_us` is the local clock at RxDone.
    pub fn on_packet(&mut self, rx_end_us: u64, packet: &[u8]) -> Result<(), wire::Error> {
        match wire::decode(packet)? {
            wire::Message::Beacon(beacon) => {
                let len = u8::try_from(packet.len()).unwrap_or(u8::MAX);
                let airtime_us = self.modulation.packet_airtime_us(len);
                self.sync.on_beacon(rx_end_us, airtime_us, &beacon);
            }
            wire::Message::Hello(hello) => self.coloring.on_hello(rx_end_us, &hello),
        }
        Ok(())
    }

    /// Decide the next radio action. Deterministic for a given state and
    /// frame, so calling it repeatedly while waiting is safe.
    pub fn next_action(&mut self, now_us: u64) -> Action {
        let Some(position) = self.sync.position(now_us) else {
            self.synced_since = None;
            return Action::Listen {
                revisit_us: now_us + self.config.frame_us(),
            };
        };
        let synced_since = *self.synced_since.get_or_insert(position.frame_number);
        let slot_us = self.config.slot_us();
        let frame_us = self.config.frame_us();
        let frame_start = now_us - position.offset_us - u64::from(position.slot) * slot_us;
        let relay = self
            .sync
            .beacon(now_us)
            .map(|(slot, beacon)| (slot, beacon.stratum));

        for frame_delta in 0..2u64 {
            let frame_number = position.frame_number + frame_delta;
            let base = frame_start + frame_delta * frame_us;
            let mut best: Option<(u64, Payload)> = None;

            if let Some((slot, stratum)) = relay
                && (stratum == 0 || mix(self.seed, frame_number, SALT_RELAY) & 1 == 0)
            {
                let at_us = base + u64::from(slot) * slot_us + self.config.guard_us();
                if at_us >= now_us {
                    best = Some((at_us, Payload::Beacon));
                }
            }

            if frame_number >= synced_since + LISTEN_FRAMES
                && let Some(slot) = self.hello_slot(now_us, frame_number)
            {
                let at_us = base + u64::from(slot) * slot_us + self.config.guard_us();
                if at_us >= now_us && best.as_ref().is_none_or(|(t, _)| at_us < *t) {
                    best = Some((at_us, Payload::Hello));
                }
            }

            if let Some((at_us, payload)) = best {
                let built = match payload {
                    Payload::Beacon => self.build_beacon(at_us),
                    Payload::Hello => self.build_hello(),
                };
                if built {
                    return Action::Transmit { at_us };
                }
            }
        }
        Action::Listen {
            revisit_us: frame_start + frame_us,
        }
    }

    /// The packet scheduled by the most recent [`Action::Transmit`].
    pub fn packet(&self) -> &[u8] {
        &self.tx_buf[..self.tx_len]
    }

    pub fn position(&self, now_us: u64) -> Option<FramePosition> {
        self.sync.position(now_us)
    }

    /// This node's claimed data slot, if any.
    pub fn slot(&self) -> Option<u16> {
        self.coloring.slot()
    }

    fn hello_slot(&mut self, now_us: u64, frame_number: u64) -> Option<u16> {
        if let Some(slot) = self.coloring.pick_slot(now_us) {
            return Some(slot);
        }
        // every data slot in the neighborhood is taken: fall back to hellos in
        // a random contention slot so the mesh still learns we exist
        if mix(self.seed, frame_number, SALT_CONTENTION_SEND) & 1 != 0 {
            return None;
        }
        let first = self.config.beacon_slots();
        let count = u64::from(self.config.first_data_slot() - first);
        if count == 0 {
            return None;
        }
        let pick = mix(self.seed, frame_number, SALT_CONTENTION_SLOT) % count;
        Some(first + pick as u16)
    }

    fn build_beacon(&mut self, at_us: u64) -> bool {
        let Some((_, beacon)) = self.sync.beacon(at_us) else {
            return false;
        };
        match wire::encode(&wire::Message::Beacon(beacon), &mut self.tx_buf) {
            Ok(packet) => {
                self.tx_len = packet.len();
                true
            }
            Err(_) => false,
        }
    }

    fn build_hello(&mut self) -> bool {
        let mut hello = self.coloring.hello();
        loop {
            if let Ok(packet) = wire::encode(&wire::Message::Hello(hello.clone()), &mut self.tx_buf)
                && packet.len() <= usize::from(self.max_packet_len)
            {
                self.tx_len = packet.len();
                return true;
            }
            if hello.neighbors.pop().is_none() {
                return false;
            }
        }
    }
}

/// splitmix64 finalizer: a stable per-frame coin so repeated `next_action`
/// calls within one frame agree with each other.
fn mix(seed: u64, frame_number: u64, salt: u64) -> u64 {
    let mut z = seed
        ^ frame_number.wrapping_mul(0x9e37_79b9_7f4a_7c15)
        ^ salt.wrapping_mul(0xd1b5_4a32_d192_ed03);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tdma::SlotKind;

    const FRAME_US: u64 = 16_000_000;

    fn engine(id: u32, seed: u64) -> Engine {
        Engine::new(Config::default(), Modulation::default(), NodeId(id), seed).unwrap()
    }

    #[test]
    fn unsynced_listens() {
        let mut e = engine(1, 7);
        assert_eq!(
            e.next_action(5_000_000),
            Action::Listen {
                revisit_us: 5_000_000 + FRAME_US,
            }
        );
    }

    #[test]
    fn rejects_unworkable_slot_budget() {
        // 150 slots of 40 ms = 6 s frame; 10 ms budget fits no packet
        let config = Config::new(40_000, 150, 15_000, 1, 1).unwrap();
        assert!(matches!(
            Engine::new(config, Modulation::default(), NodeId(1), 7),
            Err(Error::SlotTooShort { .. })
        ));
    }

    /// Drive an engine alone (nothing to hear), collecting its transmissions.
    fn run_solo(e: &mut Engine, start_us: u64, frames: u64) -> heapless::Vec<(u64, u16), 64> {
        let mut sent = heapless::Vec::new();
        let mut now = start_us;
        let end = start_us + frames * FRAME_US;
        while now < end {
            let next_gps = (now / 1_000_000 + 1) * 1_000_000;
            match e.next_action(now) {
                Action::Transmit { at_us } if at_us < next_gps => {
                    let slot = e.position(at_us).unwrap().slot;
                    if Config::default().slot_kind(slot) == SlotKind::Data {
                        assert!(matches!(
                            wire::decode(e.packet()),
                            Ok(wire::Message::Hello(h)) if h.slot == e.slot()
                        ));
                    }
                    sent.push((at_us, slot)).unwrap();
                    now = at_us
                        + Modulation::default()
                            .packet_airtime_us(u8::try_from(e.packet().len()).unwrap());
                }
                _ => {
                    e.on_gps_second(next_gps, next_gps / 1_000_000 + 15_980);
                    now = next_gps;
                }
            }
        }
        sent
    }

    #[test]
    fn root_beacons_then_claims_after_listen_window() {
        let mut e = engine(1, 7);
        let config = Config::default();
        let sent = run_solo(&mut e, 20_000_000, 6);

        // synced mid-frame at the first gps second, so the first beacon lands
        // in the following frame: 5 beacons within the 6-frame window
        let beacons: heapless::Vec<u64, 64> = sent
            .iter()
            .filter(|(_, slot)| *slot == 0)
            .map(|(at, _)| *at)
            .collect();
        assert_eq!(beacons.len(), 5);
        for pair in beacons.windows(2) {
            assert_eq!(pair[1] - pair[0], FRAME_US);
        }
        assert_eq!(beacons[0] % FRAME_US, config.guard_us() + 4_000_000);

        let hellos: heapless::Vec<(u64, u16), 64> = sent
            .iter()
            .filter(|(_, slot)| config.slot_kind(*slot) == SlotKind::Data)
            .copied()
            .collect();
        assert!(!hellos.is_empty());
        assert_eq!(e.slot(), Some(hellos[0].1));
        // listen window: no data-slot claim in the first two synced frames
        let first_synced_frame_start = beacons[0] - config.guard_us() - FRAME_US;
        assert!(hellos[0].0 >= first_synced_frame_start + LISTEN_FRAMES * FRAME_US);
    }

    #[test]
    fn two_nodes_converge_without_collisions() {
        let mut a = engine(1, 0xAAAA);
        let mut b = engine(2, 0xBBBB);
        let modulation = Modulation::default();
        let start = 20_000_000u64;
        let end = start + 25 * FRAME_US;
        let mut now = start;
        let mut collisions = 0u32;
        let mut b_heard = 0u32;

        while now < end {
            let action_a = a.next_action(now);
            let action_b = b.next_action(now);
            let time = |action: Action| match action {
                Action::Transmit { at_us } => at_us,
                Action::Listen { revisit_us } => revisit_us,
            };
            let t_gps = (now / 1_000_000 + 1) * 1_000_000;
            let t = t_gps.min(time(action_a)).min(time(action_b));
            now = t;
            if t == t_gps {
                a.on_gps_second(now, now / 1_000_000 + 15_980);
                continue;
            }
            let a_tx = t == time(action_a) && matches!(action_a, Action::Transmit { .. });
            let b_tx = t == time(action_b) && matches!(action_b, Action::Transmit { .. });
            if a_tx && b_tx {
                collisions += 1;
                now += 1;
                continue;
            }
            let (sender, receiver) = if a_tx {
                (&mut a, &mut b)
            } else {
                (&mut b, &mut a)
            };
            let mut copy = [0u8; 255];
            let len = sender.packet().len();
            copy[..len].copy_from_slice(sender.packet());
            let airtime = modulation.packet_airtime_us(u8::try_from(len).unwrap());
            receiver.on_packet(t + airtime, &copy[..len]).unwrap();
            if !a_tx {
                b_heard += 1;
            }
            now = t + airtime;
        }

        assert_eq!(collisions, 0);
        assert!(b_heard > 0);
        assert!(a.position(now).is_some());
        assert!(b.position(now).is_some());
        let (slot_a, slot_b) = (a.slot().unwrap(), b.slot().unwrap());
        assert_ne!(slot_a, slot_b);
        // both agree on the timeline exactly (perfect channel)
        assert_eq!(
            a.position(now).unwrap().frame_number,
            b.position(now).unwrap().frame_number
        );
        assert_eq!(
            a.position(now).unwrap().offset_us,
            b.position(now).unwrap().offset_us
        );
    }
}
