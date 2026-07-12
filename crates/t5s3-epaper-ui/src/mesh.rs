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
/// while the lora page is open.
const SLICE_BUDGET_US: u64 = 20_000;

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
    last_utc_fed: u64,
}

impl Mesh {
    pub(crate) fn new() -> Result<Self, nootmesh::tdma::engine::Error> {
        // stable per-board identity from the efuse mac; trng entropy for the
        // engine's randomized skips and root-fallback jitter.
        let mac =
            esp_hal::efuse::interface_mac_address(esp_hal::efuse::InterfaceMacAddress::Station);
        let m = mac.as_bytes();
        let node_id = NodeId(u32::from_be_bytes([m[2], m[3], m[4], m[5]]));
        let rng = esp_hal::rng::Rng::new();
        let seed = (u64::from(rng.random()) << 32) | u64::from(rng.random());
        let engine = Engine::new(
            nootmesh::tdma::Config::default(),
            Modulation::default(),
            node_id,
            seed,
        )?;
        Ok(Self {
            engine,
            node_id: node_id.0,
            rx_count: 0,
            tx_count: 0,
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

    pub(crate) fn max_text_len(&self) -> usize {
        self.engine.max_text_len()
    }

    pub(crate) fn queue_text(&mut self, body: &[u8]) -> Result<(), QueueError> {
        self.engine.queue_text(body)
    }

    /// Next received chat text as `(sender id, lossy utf-8 text)`.
    pub(crate) fn take_text(&mut self) -> Option<(u32, String)> {
        self.engine.take_text().map(|(from, body)| {
            let text = match core::str::from_utf8(&body) {
                Ok(s) => String::from(s),
                Err(_) => String::from("<binary>"),
            };
            (from.0, text)
        })
    }

    /// One-line mesh status for the lora page.
    pub(crate) fn status_line(&self) -> String {
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
        format!(
            "{role} | {slot} {frame} | rx {} tx {}",
            self.rx_count, self.tx_count
        )
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
                            esp_println::println!("mesh tx text {}B", t.body.len())
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
                    if was_stale {
                        if let Ok(wire::Message::Beacon(_)) = wire::decode(&buf[..n]) {
                            esp_println::println!("mesh: dropped stale-timestamped beacon");
                            continue;
                        }
                    }
                    match self.engine.on_packet(t, &buf[..n]) {
                        Ok(()) => esp_println::println!(
                            "mesh rx {n}B rssi {} dBm snr {} dB",
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
