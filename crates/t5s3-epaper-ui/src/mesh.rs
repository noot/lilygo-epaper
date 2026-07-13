//! nootmesh servicing for the lora page.
//!
//! The main loop's ~50 ms pass cadence is far too coarse for TDMA: transmits
//! must start at a slot boundary plus guard, and a beacon's RxDone timestamp
//! feeds timeline recovery, both wanting millisecond precision. So each pass
//! hands the mesh a short *servicing slice*: a tight poll loop with precise
//! timestamps, which also transmits when a deadline falls near it. Packets
//! that arrived between slices carry stale timestamps, so beacons found by
//! the slice's first poll are dropped rather than corrupting the timeline
//! (hellos and texts carry no timing and are always accepted).

use alloc::{format, string::String};

use nootmesh::{
    airtime::Modulation,
    tdma::{engine::QueueError, Action, Engine},
    wire,
    NodeId,
};
use t5s3_epaper_core::lora::Lora;

/// rx-polling budget per ui pass; touch latency grows by about this much
/// while the lora page is open. Sized so beacons have a decent chance of
/// landing inside an active slice (packets latched between slices carry
/// stale timestamps, so beacons among them must be discarded).
const SLICE_BUDGET_US: u64 = 30_000;

/// a tx deadline within this horizon is waited for inside the slice rather
/// than deferred to a later pass (which could overshoot the slot's guard).
const TX_WAIT_HORIZON_US: u64 = 120_000;

fn now_us() -> u64 {
    esp_hal::time::Instant::now()
        .duration_since_epoch()
        .as_micros()
}

/// One node's mesh membership, alive only while the lora page is open
/// (mirroring the radio's lifecycle).
pub(crate) struct Mesh {
    engine: Engine,
    node_id: u32,
    rx_count: u32,
    tx_count: u32,
    last_rssi_dbm: Option<i16>,
    last_utc_fed: u64,
}

impl Mesh {
    /// `store`: retain recent texts and answer recap requests, like the
    /// relay nodes do — sensible only with the always-on radio setting, since
    /// a store node that naps is a mailbox nobody can reach.
    pub(crate) fn new(store: bool) -> Result<Self, nootmesh::tdma::engine::Error> {
        // stable per-board identity from the efuse mac; trng entropy for the
        // engine's randomized skips and root-fallback jitter.
        let mac =
            esp_hal::efuse::interface_mac_address(esp_hal::efuse::InterfaceMacAddress::Station);
        let m = mac.as_bytes();
        let node_id = NodeId(u32::from_be_bytes([m[2], m[3], m[4], m[5]]));
        let rng = esp_hal::rng::Rng::new();
        let seed = (u64::from(rng.random()) << 32) | u64::from(rng.random());
        let mut engine = Engine::new(
            nootmesh::tdma::Config::default(),
            Modulation::default(),
            node_id,
            seed,
        )?;
        if store {
            engine.enable_store();
        }
        // catch up on chat missed while off-mesh: store nodes in range replay
        // their retained history, and dedup drops what we already saw
        engine.request_recap();
        Ok(Self {
            engine,
            node_id: node_id.0,
            rx_count: 0,
            tx_count: 0,
            last_rssi_dbm: None,
            last_utc_fed: 0,
        })
    }

    pub(crate) fn node_id(&self) -> u32 {
        self.node_id
    }

    /// Feed a UTC second freshly parsed from the gps (call only right after a
    /// sentence arrived: a stale value would step the mesh timeline).
    pub(crate) fn on_gps_second(&mut self, utc_seconds: u64) {
        if utc_seconds != self.last_utc_fed {
            self.last_utc_fed = utc_seconds;
            self.engine.on_gps_second(now_us(), utc_seconds);
        }
    }

    /// Set (or change) the flooded display name; empty clears nothing on
    /// peers but stops re-announcing.
    pub(crate) fn set_alias(&mut self, name: &str) {
        if name.is_empty() {
            return;
        }
        if let Err(e) = self.engine.set_alias(now_us(), name.as_bytes()) {
            esp_println::println!("mesh: set alias failed: {e}");
        }
    }

    /// "name (137c)" when a name claim is known for `id`, else the bare hex
    /// id. The id tail stays visible because names are unauthenticated
    /// claims.
    pub(crate) fn display_name(&self, id: u32) -> String {
        let claimed = if id == self.node_id {
            self.engine.alias()
        } else {
            self.engine.alias_of(nootmesh::NodeId(id))
        };
        match claimed.and_then(|bytes| core::str::from_utf8(bytes).ok()) {
            Some(name) => format!("{name} ({:04x})", id & 0xffff),
            None => format!("{id:08x}"),
        }
    }

    pub(crate) fn max_text_len(&self) -> usize {
        self.engine.max_text_len()
    }

    pub(crate) fn queue_text(&mut self, body: &[u8]) -> Result<(), QueueError> {
        self.engine.queue_text(now_us(), body)
    }

    /// Next received chat text as `(author id, origination utc, lossy utf-8)`.
    pub(crate) fn take_text(&mut self) -> Option<(u32, Option<u64>, String)> {
        self.engine.take_text().map(|incoming| {
            let text = match core::str::from_utf8(&incoming.body) {
                Ok(s) => String::from(s),
                Err(_) => String::from("<binary>"),
            };
            (incoming.from.0, incoming.utc_seconds, text)
        })
    }

    /// The parts of the status whose change warrants an immediate display
    /// refresh: role/root/stratum and the slot claim. Counters and the frame
    /// number are excluded — refreshing the e-paper blocks the radio for
    /// hundreds of milliseconds, so flushing on every received packet would
    /// deafen the node (enough to make two contending roots never hear each
    /// other's beacons).
    pub(crate) fn status_key(&self) -> (Option<(u32, u8)>, Option<u16>, usize) {
        let now = now_us();
        (
            self.engine
                .root(now)
                .map(|(root, stratum)| (root.0, stratum)),
            self.engine.slot(),
            // peers joining/leaving radio range warrant an immediate flush:
            // it is the range-testing signal
            self.engine.peer_count(now),
        )
    }

    /// The info tab's own-node header: identity/role, timeline, counters.
    pub(crate) fn info_lines(&self) -> [String; 3] {
        let now = now_us();
        let role = match self.engine.root(now) {
            Some((root, _)) if root.0 == self.node_id => String::from("ROOT"),
            Some((root, stratum)) => format!("root {:08x} s{stratum}", root.0),
            None => String::from("syncing..."),
        };
        let slot = match self.engine.slot() {
            Some(slot) => format!("slot {slot}"),
            None => String::from("slot --"),
        };
        let frame = match self.engine.position(now) {
            Some(p) => format!("f {}", p.frame_number),
            None => String::new(),
        };
        let rssi = match self.last_rssi_dbm {
            Some(dbm) => format!(" | last rx {dbm}dBm"),
            None => String::new(),
        };
        [
            format!("{} | {role}", self.display_name(self.node_id)),
            format!("{slot} {frame} | peers {}", self.engine.peer_count(now)),
            format!(
                "rx {} tx {} | store {}{rssi}",
                self.rx_count,
                self.tx_count,
                self.engine.store_len()
            ),
        ]
    }

    /// The info tab's peer table, one formatted row per known peer: direct
    /// peers with link quality and age, gossip-only peers with their source.
    pub(crate) fn peer_rows(&self) -> alloc::vec::Vec<String> {
        let now = now_us();
        let mut rows = alloc::vec::Vec::new();
        for peer in self.engine.peers(now) {
            let name = self.display_name(peer.id.0);
            let slot = match peer.slot {
                Some(slot) => format!("{slot}"),
                None => String::from("--"),
            };
            let row = match (peer.rssi_dbm, peer.heard_age_us, peer.via) {
                (Some(dbm), Some(age), _) => {
                    let age_s = age / 1_000_000;
                    format!("{name:<22}{slot:<7}{dbm:<6}{age_s}s ago")
                }
                (_, _, Some(via)) => {
                    format!("{name:<22}{slot:<7}via {}", self.display_name(via.0))
                }
                _ => format!("{name:<22}{slot}"),
            };
            rows.push(row);
        }
        rows
    }

    /// Service the mesh for one ui pass: receive with precise timestamps for
    /// [`SLICE_BUDGET_US`], transmitting when a deadline falls within reach.
    pub(crate) fn service(&mut self, radio: &mut Lora<'_, 'static>) {
        let slice_end = now_us() + SLICE_BUDGET_US;
        let mut stale = true;
        let mut buf = [0u8; 255];
        loop {
            let now = now_us();
            match self.engine.next_action(now) {
                Action::Transmit { at_us } if at_us <= now + TX_WAIT_HORIZON_US => {
                    if self.poll_until(radio, &mut buf, at_us, &mut stale) {
                        continue; // a packet may have rescheduled us
                    }
                    match wire::decode(self.engine.packet()) {
                        Ok(wire::Message::Beacon(b)) => {
                            esp_println::println!(
                                "mesh tx beacon s{} f{}",
                                b.stratum,
                                b.frame_number
                            )
                        }
                        Ok(wire::Message::Hello(h)) => {
                            esp_println::println!("mesh tx hello slot {:?}", h.slot)
                        }
                        Ok(wire::Message::Text(t)) => {
                            esp_println::println!(
                                "mesh tx text {}B from {:08x} hops {}",
                                t.body.len(),
                                t.origin.0,
                                t.hops
                            )
                        }
                        Ok(wire::Message::Recap(_)) => {
                            esp_println::println!("mesh tx recap request")
                        }
                        Ok(wire::Message::Alias(a)) => {
                            esp_println::println!(
                                "mesh tx alias from {:08x} hops {}",
                                a.origin.0,
                                a.hops
                            )
                        }
                        Err(_) => {}
                    }
                    match radio.transmit(self.engine.packet()) {
                        Ok(()) => {
                            self.engine.on_transmitted();
                            self.tx_count = self.tx_count.wrapping_add(1);
                        }
                        Err(e) => esp_println::println!("mesh tx error: {e}"),
                    }
                    if let Err(e) = radio.start_receive() {
                        esp_println::println!("mesh start rx error: {e}");
                    }
                }
                _ => {
                    if self.poll_until(radio, &mut buf, slice_end, &mut stale) {
                        continue;
                    }
                    return;
                }
            }
            if now_us() >= slice_end {
                return;
            }
        }
    }

    /// Poll for packets until `deadline_us`. Returns true when one was fed to
    /// the engine (the caller should re-plan its next action).
    fn poll_until(
        &mut self,
        radio: &mut Lora<'_, 'static>,
        buf: &mut [u8; 255],
        deadline_us: u64,
        stale: &mut bool,
    ) -> bool {
        let delay = esp_hal::delay::Delay::new();
        loop {
            // timestamp BEFORE the spi reads: dio1 latched at RxDone, at most
            // one poll period before this instant (except the slice's first
            // poll, which may find a packet from long ago)
            let t = now_us();
            if t >= deadline_us {
                return false;
            }
            let was_stale = *stale;
            *stale = false;
            match radio.poll_receive(buf) {
                Ok(Some(n)) => {
                    self.rx_count = self.rx_count.wrapping_add(1);
                    self.last_rssi_dbm = Some(radio.rssi());
                    let rssi = radio.rssi();
                    if was_stale {
                        if let Ok(wire::Message::Beacon(_)) = wire::decode(&buf[..n]) {
                            esp_println::println!("mesh: dropped stale-timestamped beacon");
                            continue;
                        }
                    }
                    match self.engine.on_packet(t, &buf[..n], rssi) {
                        Ok(received) => esp_println::println!(
                            "mesh rx {n}B {received} rssi {} dBm snr {} dB",
                            radio.rssi(),
                            radio.snr()
                        ),
                        Err(e) => esp_println::println!("mesh rx {n}B undecodable: {e}"),
                    }
                    return true;
                }
                Ok(None) => {}
                Err(e) => esp_println::println!("mesh rx error: {e}"),
            }
            delay.delay_micros(50);
        }
    }
}
