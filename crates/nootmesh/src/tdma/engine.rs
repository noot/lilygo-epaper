use super::{Coloring, Config, FramePosition, Hello, Sync, mix, sync::beacon_tx_jitter_us};
use crate::{NodeId, airtime::Modulation, wire};

/// Frames to listen after first syncing before claiming a data slot, so the
/// neighbor table informs the first pick.
const LISTEN_FRAMES: u64 = 2;

/// Smallest packet capacity a slot must fit (beacons are 13 bytes).
const MIN_PACKET_LEN: u8 = 16;

const MAX_PACKET: usize = 255;

const SALT_RELAY: u64 = 1;
const SALT_CONTENTION_SEND: u64 = 2;
const SALT_CONTENTION_SLOT: u64 = 3;
const SALT_ROOT_FALLBACK: u64 = 4;
const SALT_MSG_ID: u64 = 5;

/// Texts stop being re-forwarded once they have been relayed this many times,
/// bounding both flood traffic and the mesh diameter chat can cross.
const MAX_TEXT_HOPS: u8 = 3;

/// Recently seen `(origin, msg_id)` pairs for flood dedup. Sized so an entry
/// comfortably outlives its message's bounded flood lifetime.
const SEEN_CAP: usize = 32;

/// Pending outgoing texts: this node's own plus forwards awaiting its data
/// slot. Forwards are dropped when full; own messages report
/// [`QueueError::Busy`].
const OUTBOX_CAP: usize = 4;

/// Unsynced nodes listen 2..=5 frames (randomized per node, so simultaneous
/// cold boots elect one winner) before self-appointing as a free-running
/// root. Covers meshes with no GPS anywhere in reach. A sub-frame jitter is
/// added on top: two roots whose origins were congruent mod the frame length
/// would collide beacon-slot-on-beacon-slot every frame and never hear each
/// other, so their slot grids must not align.
const ROOT_FALLBACK_MIN_FRAMES: u64 = 2;
const ROOT_FALLBACK_JITTER_FRAMES: u64 = 4;

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
        "slot budget of {budget_us} us cannot fit a {MIN_PACKET_LEN}-byte packet ({airtime_us} us) plus beacon jitter headroom"
    )]
    SlotTooShort { budget_us: u64, airtime_us: u64 },
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum QueueError {
    #[error("text of {len} bytes exceeds the slot's {max}-byte budget")]
    TooLong { len: usize, max: usize },
    #[error("the outgoing text queue is full")]
    Busy,
}

/// What a received packet contained, for callers' logs and displays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Received {
    Beacon {
        root: NodeId,
        stratum: u8,
    },
    Hello {
        sender: NodeId,
    },
    /// A new text, delivered to the inbox (and queued for forwarding when
    /// under the hop cap).
    Text {
        origin: NodeId,
        hops: u8,
    },
    /// An already-seen copy (flood duplicate or an echo of this node's own
    /// message); dropped after refreshing the transmitter's slot claim.
    DuplicateText {
        origin: NodeId,
        hops: u8,
    },
}

impl core::fmt::Display for Received {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Beacon { root, stratum } => write!(f, "beacon root {:08x} s{stratum}", root.0),
            Self::Hello { sender } => write!(f, "hello from {:08x}", sender.0),
            Self::Text { origin, hops } => write!(f, "text from {:08x} hops {hops}", origin.0),
            Self::DuplicateText { origin, hops } => {
                write!(f, "duplicate text from {:08x} hops {hops}", origin.0)
            }
        }
    }
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
    node_id: NodeId,
    seed: u64,
    sync: Sync,
    coloring: Coloring,
    synced_since: Option<u64>,
    unsynced_since: Option<u64>,
    root_fallback_us: u64,
    max_packet_len: u8,
    max_text_len: usize,
    next_msg_id: u16,
    outbox: heapless::Deque<Outgoing, OUTBOX_CAP>,
    outbox_in_tx_buf: bool,
    seen: heapless::Deque<(NodeId, u16), SEEN_CAP>,
    inbox: heapless::Deque<(NodeId, heapless::Vec<u8, { wire::TEXT_CAP }>), 4>,
    tx_buf: [u8; MAX_PACKET],
    tx_len: usize,
}

struct Outgoing {
    origin: NodeId,
    msg_id: u16,
    hops: u8,
    body: heapless::Vec<u8, { wire::TEXT_CAP }>,
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
        // beacons ride up to half the budget of transmit jitter (see
        // `beacon_tx_jitter_us`), so they must fit in the other half
        if 2 * modulation.packet_airtime_us(MIN_PACKET_LEN) > budget_us {
            return Err(Error::SlotTooShort {
                budget_us,
                airtime_us: modulation.packet_airtime_us(MIN_PACKET_LEN),
            });
        }
        let max_packet_len = (MIN_PACKET_LEN..=u8::MAX)
            .take_while(|len| modulation.packet_airtime_us(*len) <= budget_us)
            .last()
            .unwrap_or(MIN_PACKET_LEN);
        let seed = seed ^ u64::from(node_id.0);
        let fallback_frames = ROOT_FALLBACK_MIN_FRAMES
            + mix(seed, 0, SALT_ROOT_FALLBACK) % ROOT_FALLBACK_JITTER_FRAMES;
        let fallback_jitter_us = mix(seed, 1, SALT_ROOT_FALLBACK) % config.frame_us();
        // probe how many packet bytes a text's framing costs (worst-case
        // varints, neighbors trimmed away) to bound queueable body length
        let probe = wire::Message::Text(wire::Text {
            hello: Hello {
                sender: node_id,
                slot: Some(255),
                neighbors: heapless::Vec::new(),
            },
            origin: NodeId(u32::MAX),
            msg_id: u16::MAX,
            hops: u8::MAX,
            body: heapless::Vec::new(),
        });
        let mut probe_buf = [0u8; MAX_PACKET];
        let overhead = match wire::encode(&probe, &mut probe_buf) {
            Ok(packet) => packet.len(),
            Err(_) => usize::from(max_packet_len),
        };
        let max_text_len = usize::from(max_packet_len)
            .saturating_sub(overhead)
            .min(wire::TEXT_CAP);
        Ok(Self {
            config,
            modulation,
            node_id,
            seed,
            sync: Sync::new(config, node_id),
            coloring: Coloring::new(config, node_id),
            synced_since: None,
            unsynced_since: None,
            root_fallback_us: fallback_frames * config.frame_us() + fallback_jitter_us,
            max_packet_len,
            max_text_len,
            next_msg_id: mix(seed, 0, SALT_MSG_ID) as u16,
            outbox: heapless::Deque::new(),
            outbox_in_tx_buf: false,
            seen: heapless::Deque::new(),
            inbox: heapless::Deque::new(),
            tx_buf: [0; MAX_PACKET],
            tx_len: 0,
        })
    }

    /// Feed a UTC second boundary from GPS while holding a fix.
    pub fn on_gps_second(&mut self, now_us: u64, utc_seconds: u64) {
        self.sync.on_gps_second(now_us, utc_seconds);
    }

    /// Feed a received packet. `rx_end_us` is the local clock at RxDone.
    /// Returns what the packet contained, for logs and displays.
    pub fn on_packet(&mut self, rx_end_us: u64, packet: &[u8]) -> Result<Received, wire::Error> {
        match wire::decode(packet)? {
            wire::Message::Beacon(beacon) => {
                let len = u8::try_from(packet.len()).unwrap_or(u8::MAX);
                let airtime_us = self.modulation.packet_airtime_us(len);
                self.sync.on_beacon(rx_end_us, airtime_us, &beacon);
                Ok(Received::Beacon {
                    root: beacon.root,
                    stratum: beacon.stratum,
                })
            }
            wire::Message::Hello(hello) => {
                self.coloring.on_hello(rx_end_us, &hello);
                Ok(Received::Hello {
                    sender: hello.sender,
                })
            }
            wire::Message::Text(text) => {
                let duplicate = Ok(Received::DuplicateText {
                    origin: text.origin,
                    hops: text.hops,
                });
                if text.hello.sender == self.node_id {
                    return duplicate;
                }
                // the transmitter's embedded hello is fresh claim info even
                // when the message itself is an already-seen duplicate
                self.coloring.on_hello(rx_end_us, &text.hello);
                let key = (text.origin, text.msg_id);
                if text.origin == self.node_id || self.seen.iter().any(|seen| *seen == key) {
                    return duplicate;
                }
                self.mark_seen(key);
                if self.inbox.is_full() {
                    self.inbox.pop_front();
                }
                let _ = self.inbox.push_back((text.origin, text.body.clone()));
                // flood: re-broadcast once in our own data slot, up to the
                // hop cap; a full outbox just drops the forward
                if text.hops < MAX_TEXT_HOPS {
                    let _ = self.outbox.push_back(Outgoing {
                        origin: text.origin,
                        msg_id: text.msg_id,
                        hops: text.hops + 1,
                        body: text.body,
                    });
                }
                Ok(Received::Text {
                    origin: text.origin,
                    hops: text.hops,
                })
            }
        }
    }

    fn mark_seen(&mut self, key: (NodeId, u16)) {
        if self.seen.is_full() {
            self.seen.pop_front();
        }
        let _ = self.seen.push_back(key);
    }

    /// Largest text body that fits a data slot alongside the embedded hello.
    pub fn max_text_len(&self) -> usize {
        self.max_text_len
    }

    /// Queue a text authored by this node, flooded from its next data slot.
    /// The queue is shared with forwards; entries free when
    /// [`on_transmitted`](Self::on_transmitted) confirms they went on the air.
    pub fn queue_text(&mut self, body: &[u8]) -> Result<(), QueueError> {
        if body.len() > self.max_text_len {
            return Err(QueueError::TooLong {
                len: body.len(),
                max: self.max_text_len,
            });
        }
        if self.outbox.is_full() {
            return Err(QueueError::Busy);
        }
        let mut queued = heapless::Vec::new();
        // bounded by max_text_len <= TEXT_CAP above, so this cannot overflow
        let _ = queued.extend_from_slice(body);
        let msg_id = self.next_msg_id;
        self.next_msg_id = self.next_msg_id.wrapping_add(1);
        // so relayed echoes of our own message are ignored
        self.mark_seen((self.node_id, msg_id));
        let _ = self.outbox.push_back(Outgoing {
            origin: self.node_id,
            msg_id,
            hops: 0,
            body: queued,
        });
        Ok(())
    }

    /// Confirm the packet from the most recent [`Action::Transmit`] went on
    /// the air. Without this a queued text is rebuilt into the next data
    /// slot rather than released.
    pub fn on_transmitted(&mut self) {
        if self.outbox_in_tx_buf {
            self.outbox.pop_front();
            self.outbox_in_tx_buf = false;
        }
    }

    /// Next received text as `(author, body)`, oldest first. Flood
    /// deduplication already happened: each message is delivered once no
    /// matter how many relayed copies arrive.
    pub fn take_text(&mut self) -> Option<(NodeId, heapless::Vec<u8, { wire::TEXT_CAP }>)> {
        self.inbox.pop_front()
    }

    /// Decide the next radio action. Deterministic for a given state and
    /// frame, so calling it repeatedly while waiting is safe.
    pub fn next_action(&mut self, now_us: u64) -> Action {
        let position = match self.sync.position(now_us) {
            Some(position) => {
                self.unsynced_since = None;
                position
            }
            None => {
                self.synced_since = None;
                let since = *self.unsynced_since.get_or_insert(now_us);
                let deadline = since + self.root_fallback_us;
                if now_us < deadline {
                    return Action::Listen {
                        revisit_us: deadline.min(now_us + self.config.frame_us()),
                    };
                }
                self.unsynced_since = None;
                self.sync.become_root(now_us);
                let Some(position) = self.sync.position(now_us) else {
                    // become_root always syncs; defensive fallback
                    return Action::Listen {
                        revisit_us: now_us + self.config.frame_us(),
                    };
                };
                position
            }
        };
        let synced_since = *self.synced_since.get_or_insert(position.frame_number);
        let slot_us = self.config.slot_us();
        let frame_us = self.config.frame_us();
        let frame_start = now_us - position.offset_us - u64::from(position.slot) * slot_us;
        let relay = self
            .sync
            .beacon(now_us)
            .map(|(slot, beacon)| (slot, beacon.stratum, beacon.root));

        for frame_delta in 0..2u64 {
            let frame_number = position.frame_number + frame_delta;
            let base = frame_start + frame_delta * frame_us;
            let mut best: Option<(u64, Payload)> = None;

            if let Some((slot, stratum, root)) = relay
                && (stratum == 0 || mix(self.seed, frame_number, SALT_RELAY) & 1 == 0)
            {
                let at_us = base
                    + u64::from(slot) * slot_us
                    + self.config.guard_us()
                    + beacon_tx_jitter_us(&self.config, root, frame_number);
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

    /// Current root and this node's stratum, for status displays.
    pub fn root(&self, now_us: u64) -> Option<(NodeId, u8)> {
        self.sync.root(now_us)
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
        self.outbox_in_tx_buf = false;
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
        let pending = self
            .outbox
            .front()
            .map(|out| (out.origin, out.msg_id, out.hops, out.body.clone()));
        loop {
            let message = match &pending {
                Some((origin, msg_id, hops, body)) => wire::Message::Text(wire::Text {
                    hello: hello.clone(),
                    origin: *origin,
                    msg_id: *msg_id,
                    hops: *hops,
                    body: body.clone(),
                }),
                None => wire::Message::Hello(hello.clone()),
            };
            if let Ok(packet) = wire::encode(&message, &mut self.tx_buf)
                && packet.len() <= usize::from(self.max_packet_len)
            {
                self.tx_len = packet.len();
                self.outbox_in_tx_buf = pending.is_some();
                return true;
            }
            if hello.neighbors.pop().is_none() {
                return false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tdma::{SlotKind, test_config};

    const FRAME_US: u64 = 16_000_000;

    fn engine(id: u32, seed: u64) -> Engine {
        Engine::new(test_config(), Modulation::default(), NodeId(id), seed).unwrap()
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
                    if test_config().slot_kind(slot) == SlotKind::Data {
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
        let config = test_config();
        let sent = run_solo(&mut e, 20_000_000, 6);

        // synced mid-frame at the first gps second, so the first beacon lands
        // in the following frame: 5 beacons within the 6-frame window
        let beacons: heapless::Vec<u64, 64> = sent
            .iter()
            .filter(|(_, slot)| *slot == 0)
            .map(|(at, _)| *at)
            .collect();
        assert_eq!(beacons.len(), 5);
        // anchored at 21 s (utc 16_001), so frame 1000 began at 20 s; each
        // beacon sits at its frame start + guard + that frame's jitter
        for (k, at_us) in beacons.iter().enumerate() {
            let frame_number = 1_001 + k as u64;
            let frame_start = 20_000_000 + (frame_number - 1_000) * FRAME_US;
            let jitter = beacon_tx_jitter_us(&config, NodeId(1), frame_number);
            assert_eq!(*at_us, frame_start + config.guard_us() + jitter);
        }

        let hellos: heapless::Vec<(u64, u16), 64> = sent
            .iter()
            .filter(|(_, slot)| config.slot_kind(*slot) == SlotKind::Data)
            .copied()
            .collect();
        assert!(!hellos.is_empty());
        assert_eq!(e.slot(), Some(hellos[0].1));
        // listen window: no data-slot claim in the first two synced frames
        // (frame 1000 began at 20 s)
        assert!(hellos[0].0 >= 20_000_000 + LISTEN_FRAMES * FRAME_US);
    }

    /// step an engine to its next data-slot transmit, feeding gps seconds so
    /// the root stays alive, and return the tx time.
    fn step_to_data_tx(e: &mut Engine, mut now: u64) -> u64 {
        loop {
            let next_gps = (now / 1_000_000 + 1) * 1_000_000;
            match e.next_action(now) {
                Action::Transmit { at_us } if at_us < next_gps => {
                    let slot = e.position(at_us).unwrap().slot;
                    if test_config().slot_kind(slot) == SlotKind::Data {
                        return at_us;
                    }
                    now = at_us + 50_000;
                }
                _ => {
                    e.on_gps_second(next_gps, next_gps / 1_000_000 + 15_980);
                    now = next_gps;
                }
            }
        }
    }

    #[test]
    fn queued_text_rides_the_data_slot() {
        let mut a = engine(1, 7);
        let mut b = engine(2, 8);
        assert_eq!(a.max_text_len(), 54);
        assert_eq!(
            a.queue_text(&[0u8; 64]),
            Err(QueueError::TooLong { len: 64, max: 54 })
        );
        a.queue_text(b"hi from a").unwrap();
        a.queue_text(b"two").unwrap();
        a.queue_text(b"three").unwrap();
        a.queue_text(b"four").unwrap();
        assert_eq!(a.queue_text(b"overflow"), Err(QueueError::Busy));

        let at_us = step_to_data_tx(&mut a, 20_000_000);
        let text = match wire::decode(a.packet()) {
            Ok(wire::Message::Text(text)) => text,
            other => panic!("expected text, got {other:?}"),
        };
        assert_eq!(text.body.as_slice(), b"hi from a");
        assert_eq!(text.origin, NodeId(1));
        assert_eq!(text.hops, 0);
        assert_eq!(text.hello.sender, NodeId(1));
        assert_eq!(text.hello.slot, a.slot());

        // a text still refreshes the slot claim and lands in the inbox
        b.on_packet(at_us + 60_000, a.packet()).unwrap();
        let (from, body) = b.take_text().unwrap();
        assert_eq!(from, NodeId(1));
        assert_eq!(body.as_slice(), b"hi from a");
        assert_eq!(b.take_text(), None);

        // unconfirmed: rebuilt next frame; confirmed: the queue advances
        let at_us = step_to_data_tx(&mut a, at_us + 60_000);
        assert!(matches!(
            wire::decode(a.packet()),
            Ok(wire::Message::Text(t)) if t.body.as_slice() == b"hi from a"
        ));
        a.on_transmitted();
        a.on_transmitted(); // no tx since build: must NOT pop another entry
        let _ = step_to_data_tx(&mut a, at_us + 60_000);
        assert!(matches!(
            wire::decode(a.packet()),
            Ok(wire::Message::Text(t)) if t.body.as_slice() == b"two"
        ));
    }

    #[test]
    fn texts_flood_with_hop_cap_and_dedup() {
        let mut a = engine(1, 7);
        let mut b = engine(2, 8);
        let mut c = engine(3, 9);

        a.queue_text(b"flood me").unwrap();
        let at_us = step_to_data_tx(&mut a, 20_000_000);
        let mut from_a = [0u8; 255];
        let len_a = a.packet().len();
        from_a[..len_a].copy_from_slice(a.packet());

        // b delivers it once and queues a forward with the hop count bumped
        assert_eq!(
            b.on_packet(20_500_000, &from_a[..len_a]),
            Ok(Received::Text {
                origin: NodeId(1),
                hops: 0,
            })
        );
        assert!(b.take_text().is_some());
        assert_eq!(
            b.on_packet(20_600_000, &from_a[..len_a]),
            Ok(Received::DuplicateText {
                origin: NodeId(1),
                hops: 0,
            })
        );
        assert_eq!(b.take_text(), None);

        let at_b = step_to_data_tx(&mut b, 21_000_000);
        let forward = match wire::decode(b.packet()) {
            Ok(wire::Message::Text(text)) => text,
            other => panic!("expected forward, got {other:?}"),
        };
        assert_eq!(forward.origin, NodeId(1));
        assert_eq!(forward.hops, 1);
        assert_eq!(forward.hello.sender, NodeId(2));
        assert_eq!(forward.body.as_slice(), b"flood me");
        let mut from_b = [0u8; 255];
        let len_b = b.packet().len();
        from_b[..len_b].copy_from_slice(b.packet());
        b.on_transmitted();

        // the duplicate never re-enters b's outbox: next data tx is a hello
        let _ = step_to_data_tx(&mut b, at_b + 60_000);
        assert!(matches!(
            wire::decode(b.packet()),
            Ok(wire::Message::Hello(_))
        ));

        // c receives the forward attributed to the author, not the relay
        c.on_packet(21_500_000, &from_b[..len_b]).unwrap();
        assert_eq!(c.take_text().map(|(from, _)| from), Some(NodeId(1)));

        // the author ignores echoes of its own message
        assert_eq!(
            a.on_packet(at_us + 900_000, &from_b[..len_b]),
            Ok(Received::DuplicateText {
                origin: NodeId(1),
                hops: 1,
            })
        );
        assert_eq!(a.take_text(), None);

        // a text already at the hop cap is delivered but not re-forwarded
        let mut d = engine(4, 10);
        let mut body = heapless::Vec::new();
        body.extend_from_slice(b"capped").unwrap();
        let capped = wire::Message::Text(wire::Text {
            hello: Hello {
                sender: NodeId(9),
                slot: Some(11),
                neighbors: heapless::Vec::new(),
            },
            origin: NodeId(9),
            msg_id: 7,
            hops: 3,
            body,
        });
        let mut buf = [0u8; 255];
        let packet = wire::encode(&capped, &mut buf).unwrap();
        d.on_packet(20_500_000, packet).unwrap();
        assert!(d.take_text().is_some());
        let _ = step_to_data_tx(&mut d, 21_000_000);
        assert!(matches!(
            wire::decode(d.packet()),
            Ok(wire::Message::Hello(_))
        ));
    }

    #[test]
    fn gpsless_node_roots_after_fallback_listen() {
        let mut e = engine(3, 42);
        let start = 5_000_000u64;
        let mut now = start;
        let at_us = loop {
            match e.next_action(now) {
                Action::Listen { revisit_us } => {
                    assert!(revisit_us > now);
                    now = revisit_us;
                }
                Action::Transmit { at_us } => break at_us,
            }
        };
        // listened between 2 and 6 frames (whole frames + sub-frame jitter),
        // then rooted and beacons in slot 0
        assert!(at_us >= start + 2 * FRAME_US);
        assert!(at_us <= start + 7 * FRAME_US);
        assert!(matches!(
            wire::decode(e.packet()),
            Ok(wire::Message::Beacon(b)) if b.root == NodeId(3) && !b.root_has_gps
        ));
    }

    #[test]
    fn two_gpsless_nodes_converge() {
        let mut a = engine(1, 0x1111);
        let mut b = engine(2, 0x2222);
        let modulation = Modulation::default();
        let start = 5_000_000u64;
        let end = start + 25 * FRAME_US;
        let mut now = start;
        let mut heard = [0u32; 2];

        while now < end {
            let action_a = a.next_action(now);
            let action_b = b.next_action(now);
            let time = |action: Action| match action {
                Action::Transmit { at_us } => at_us,
                Action::Listen { revisit_us } => revisit_us,
            };
            let t = time(action_a).min(time(action_b));
            now = t;
            let a_tx = t == time(action_a) && matches!(action_a, Action::Transmit { .. });
            let b_tx = t == time(action_b) && matches!(action_b, Action::Transmit { .. });
            if !a_tx && !b_tx {
                continue;
            }
            if a_tx && b_tx {
                now += 1;
                continue;
            }
            let (sender, receiver, idx) = if a_tx {
                (&mut a, &mut b, 0)
            } else {
                (&mut b, &mut a, 1)
            };
            let mut copy = [0u8; 255];
            let len = sender.packet().len();
            copy[..len].copy_from_slice(sender.packet());
            let airtime = modulation.packet_airtime_us(u8::try_from(len).unwrap());
            receiver.on_packet(t + airtime, &copy[..len]).unwrap();
            heard[idx] += 1;
            now = t + airtime;
        }

        assert!(heard[0] > 0 && heard[1] > 0);
        // one free-running root won the election; both share its timeline
        let root_a = a.root(now).unwrap();
        let root_b = b.root(now).unwrap();
        assert_eq!(root_a.0, root_b.0);
        assert_eq!(root_a.1.min(root_b.1), 0);
        assert_eq!(
            a.position(now).unwrap().frame_number,
            b.position(now).unwrap().frame_number
        );
        assert_eq!(
            a.position(now).unwrap().offset_us,
            b.position(now).unwrap().offset_us
        );
        let (slot_a, slot_b) = (a.slot().unwrap(), b.slot().unwrap());
        assert_ne!(slot_a, slot_b);
    }

    /// Two GPS-fixed nodes anchor to the same UTC grid by construction, so
    /// both root at the same instant with aligned slot grids. Only the
    /// per-frame beacon jitter lets them hear each other; without it their
    /// slot-0 beacons collide every frame and both stay root forever.
    #[test]
    fn two_gps_roots_resolve_to_one() {
        let mut a = engine(1, 0x3333);
        let mut b = engine(2, 0x4444);
        let modulation = Modulation::default();
        let start = 20_000_000u64;
        let end = start + 20 * FRAME_US;
        let mut now = start;

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
                // both hold a fix on the same utc grid
                a.on_gps_second(now, now / 1_000_000 + 15_980);
                b.on_gps_second(now, now / 1_000_000 + 15_980);
                continue;
            }
            let a_tx = t == time(action_a) && matches!(action_a, Action::Transmit { .. });
            let b_tx = t == time(action_b) && matches!(action_b, Action::Transmit { .. });
            if a_tx && b_tx {
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
            sender.on_transmitted();
            now = t + airtime;
        }

        // the election resolved: the lower id kept the root, the other defers
        let root_a = a.root(now).unwrap();
        let root_b = b.root(now).unwrap();
        assert_eq!(root_a.0, NodeId(1));
        assert_eq!(root_b.0, NodeId(1));
        assert_eq!(root_a.1, 0);
        assert_eq!(root_b.1, 1);
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
