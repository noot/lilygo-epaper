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
const SALT_REPLAY_STAGGER: u64 = 7;

/// A recap request is retransmitted this many times (one data slot apart): a
/// single shot can vanish into a receiver's display-refresh deafness window,
/// and store nodes ignore repeats while a replay session is already running.
const RECAP_SENDS: u8 = 3;

/// A standing recap heartbeat (local clock): gaps that no event catches — a
/// range-degraded link that never fully dropped, packets lost to deafness —
/// heal within this period, since dedup makes redundant replays cheap and
/// suppression collapses redundant responders.
const RECAP_PERIOD_US: u64 = 30 * 60 * 1_000_000;

/// Texts a store node retains for recap replay.
const STORE_CAP: usize = 32;

/// How long a stored text stays replayable, by the store node's own
/// monotonic clock (frame numbers unmoor across root changes, so they can't
/// measure age). The effective window is this age or the ring capacity,
/// whichever runs out first.
const STORE_TTL_US: u64 = 24 * 60 * 60 * 1_000_000;

/// A store node delays its recap replay start by 1..=6 frames (seeded per
/// node), so overlapping responders desynchronize: the earliest starter's
/// replays are heard by the others, which cross those messages off and only
/// fill gaps the first responder didn't have.
const REPLAY_STAGGER_FRAMES: u64 = 6;

/// Texts stop being re-forwarded once they have been relayed this many times,
/// bounding both flood traffic and the mesh diameter chat can cross.
const MAX_TEXT_HOPS: u8 = 3;

/// Recently seen `(origin, msg_id)` pairs for flood dedup. Sized so an entry
/// comfortably outlives its message's bounded flood lifetime.
const SEEN_CAP: usize = 32;

/// Peers whose display names are remembered.
const ALIAS_TABLE_CAP: usize = 16;

/// A set alias is re-flooded this often (local clock), so late joiners learn
/// names without asking. It rides a data slot that would have carried a bare
/// hello, so the steady-state cost is only the name bytes.
const ALIAS_REANNOUNCE_US: u64 = 10 * 60 * 1_000_000;

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

/// The processing *outcome* of a received packet, for callers' logs and
/// displays — deliberately not a re-statement of [`wire::Message`]: the
/// duplicate variants depend on engine state (the seen-cache), which is why
/// no lossless conversion between the two can exist, and it carries no
/// payload bodies so logging one is free.
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
    /// A history request; a store node answers by replaying its retained
    /// texts from its data slot.
    Recap {
        from: NodeId,
    },
    /// A new display-name claim, recorded in the alias table (and forwarded
    /// when under the hop cap).
    Alias {
        origin: NodeId,
    },
    /// An already-seen alias flood copy; dropped after the hello refresh.
    DuplicateAlias {
        origin: NodeId,
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
            Self::Recap { from } => write!(f, "recap request from {:08x}", from.0),
            Self::Alias { origin } => write!(f, "alias from {:08x}", origin.0),
            Self::DuplicateAlias { origin } => {
                write!(f, "duplicate alias from {:08x}", origin.0)
            }
        }
    }
}

/// A chat text delivered to this node, exactly once per message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Incoming {
    /// The author (not the relay it may have arrived through).
    pub from: NodeId,
    /// UTC seconds at origination, when the author's mesh was GPS-anchored;
    /// `None` means the receiver only knows its own arrival time.
    pub utc_seconds: Option<u64>,
    pub body: heapless::Vec<u8, { wire::TEXT_CAP }>,
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
    tx_carries: TxCarries,
    seen: heapless::Deque<(NodeId, u16), SEEN_CAP>,
    inbox: heapless::Deque<Incoming, 4>,
    alias: Option<heapless::Vec<u8, { wire::ALIAS_CAP }>>,
    aliases: heapless::FnvIndexMap<NodeId, heapless::Vec<u8, { wire::ALIAS_CAP }>, ALIAS_TABLE_CAP>,
    alias_next_us: u64,
    store_enabled: bool,
    store: heapless::Deque<Stored, STORE_CAP>,
    replay_start_us: u64,
    recap_sends_left: u8,
    recap_next_us: u64,
    tx_buf: [u8; MAX_PACKET],
    tx_len: usize,
}

#[derive(Clone)]
struct Outgoing {
    origin: NodeId,
    msg_id: u16,
    hops: u8,
    kind: OutKind,
}

/// What an outbox entry carries: chat, or an alias announcement (own or a
/// flood-forward). Both ride the data slot and share the dedup machinery.
#[derive(Clone)]
enum OutKind {
    Text {
        timestamp: Option<u64>,
        body: heapless::Vec<u8, { wire::TEXT_CAP }>,
    },
    Alias {
        name: heapless::Vec<u8, { wire::ALIAS_CAP }>,
    },
}

struct Stored {
    origin: NodeId,
    msg_id: u16,
    timestamp: Option<u64>,
    body: heapless::Vec<u8, { wire::TEXT_CAP }>,
    /// local-clock instant this entry ages out (insertion + TTL, or a
    /// restored snapshot's remaining lifetime rebased onto this boot).
    expires_at_us: u64,
    /// awaiting replay in the current recap session.
    pending: bool,
}

/// One persisted replay-store entry. Retention travels as *remaining*
/// lifetime: neither the local microsecond clock nor mesh frame numbers
/// survive a reboot, so an absolute expiry would be meaningless on the next
/// boot's clock.
#[derive(serde::Serialize, serde::Deserialize)]
struct PersistEntry {
    origin: NodeId,
    msg_id: u16,
    timestamp: Option<u64>,
    remaining_us: u64,
    body: heapless::Vec<u8, { wire::TEXT_CAP }>,
}

/// What the packet in `tx_buf` commits to when its transmit is confirmed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TxCarries {
    Nothing,
    QueuedText,
    Replay(NodeId, u16),
    RecapRequest,
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
        // varints, neighbors trimmed away) to bound queueable body length.
        // the timestamp probe is u32::MAX (utc seconds fit u32 until 2106;
        // queue_text clamps to keep the encoding bounded)
        let probe = wire::Message::Text(wire::Text {
            hello: Hello {
                sender: node_id,
                slot: Some(255),
                neighbors: heapless::Vec::new(),
            },
            origin: NodeId(u32::MAX),
            msg_id: u16::MAX,
            hops: u8::MAX,
            timestamp: Some(u64::from(u32::MAX)),
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
            tx_carries: TxCarries::Nothing,
            seen: heapless::Deque::new(),
            inbox: heapless::Deque::new(),
            alias: None,
            aliases: heapless::FnvIndexMap::new(),
            alias_next_us: 0,
            store_enabled: false,
            store: heapless::Deque::new(),
            replay_start_us: 0,
            recap_sends_left: 0,
            recap_next_us: 0,
            tx_buf: [0; MAX_PACKET],
            tx_len: 0,
        })
    }

    /// Feed a UTC second boundary from GPS while holding a fix.
    pub fn on_gps_second(&mut self, now_us: u64, utc_seconds: u64) {
        self.sync.on_gps_second(now_us, utc_seconds);
    }

    /// Feed a received packet. `rx_end_us` is the local clock at RxDone and
    /// `rssi_dbm` its received signal strength (attributed to the
    /// *transmitter* — for a forwarded text that is the relay, whose link is
    /// what the strength describes). Returns what the packet contained, for
    /// logs and displays.
    pub fn on_packet(
        &mut self,
        rx_end_us: u64,
        packet: &[u8],
        rssi_dbm: i16,
    ) -> Result<Received, wire::Error> {
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
                self.hello_heard(rx_end_us, &hello, rssi_dbm);
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
                self.hello_heard(rx_end_us, &text.hello, rssi_dbm);
                let key = (text.origin, text.msg_id);
                // any transmission of this message (fresh flood or another
                // store node's replay) just reached everyone in range, so a
                // pending replay of it is redundant
                self.clear_pending(key);
                if text.origin == self.node_id || self.seen.iter().any(|seen| *seen == key) {
                    return duplicate;
                }
                self.mark_seen(key);
                if self.inbox.is_full() {
                    self.inbox.pop_front();
                }
                let _ = self.inbox.push_back(Incoming {
                    from: text.origin,
                    utc_seconds: text.timestamp,
                    body: text.body.clone(),
                });
                self.store_text(rx_end_us, key, text.timestamp, &text.body);
                // flood: re-broadcast once in our own data slot, up to the
                // hop cap; a full outbox just drops the forward
                if text.hops < MAX_TEXT_HOPS {
                    let _ = self.outbox.push_back(Outgoing {
                        origin: text.origin,
                        msg_id: text.msg_id,
                        hops: text.hops + 1,
                        kind: OutKind::Text {
                            timestamp: text.timestamp,
                            body: text.body,
                        },
                    });
                }
                Ok(Received::Text {
                    origin: text.origin,
                    hops: text.hops,
                })
            }
            wire::Message::Alias(alias) => {
                let duplicate = Ok(Received::DuplicateAlias {
                    origin: alias.origin,
                });
                if alias.hello.sender == self.node_id {
                    return duplicate;
                }
                self.hello_heard(rx_end_us, &alias.hello, rssi_dbm);
                let key = (alias.origin, alias.msg_id);
                if alias.origin == self.node_id || self.seen.iter().any(|seen| *seen == key) {
                    return duplicate;
                }
                self.mark_seen(key);
                // remember the claim; a full table drops new names rather
                // than evicting (bounded, cosmetic data)
                let _ = self.aliases.insert(alias.origin, alias.name.clone());
                if alias.hops < MAX_TEXT_HOPS {
                    let _ = self.outbox.push_back(Outgoing {
                        origin: alias.origin,
                        msg_id: alias.msg_id,
                        hops: alias.hops + 1,
                        kind: OutKind::Alias { name: alias.name },
                    });
                }
                Ok(Received::Alias {
                    origin: alias.origin,
                })
            }
            wire::Message::Recap(recap) => {
                let from = recap.hello.sender;
                if from == self.node_id {
                    return Ok(Received::Recap { from });
                }
                self.hello_heard(rx_end_us, &recap.hello, rssi_dbm);
                // a session already replaying serves this requester too (its
                // retransmits would otherwise restart the session from the top)
                if self.store_enabled && !self.store.iter().any(|entry| entry.pending) {
                    self.prune_store(rx_end_us);
                    if !self.store.is_empty() {
                        // the stagger deadline is local-clock, NOT a frame
                        // number: a root change can restart frame numbers
                        // near zero, which would leave a frame-based deadline
                        // minutes-to-hours in the new timeline's future
                        // (observed on hardware as a ~5-minute replay stall)
                        let stagger = 1 + mix(u64::from(self.node_id.0), 0, SALT_REPLAY_STAGGER)
                            % REPLAY_STAGGER_FRAMES;
                        self.replay_start_us = rx_end_us + stagger * self.config.frame_us();
                        for entry in self.store.iter_mut() {
                            entry.pending = true;
                        }
                    }
                }
                Ok(Received::Recap { from })
            }
        }
    }

    /// Record a heard hello; a *returning* peer (pruned for silence, now
    /// back) triggers a recap, since either side may hold messages from the
    /// time apart — the request makes them replay to us, and their own
    /// trigger makes us replay to them.
    fn hello_heard(&mut self, rx_end_us: u64, hello: &Hello, rssi_dbm: i16) {
        if self.coloring.on_hello(rx_end_us, hello, rssi_dbm) {
            self.recap_sends_left = self.recap_sends_left.max(RECAP_SENDS);
        }
    }

    fn clear_pending(&mut self, key: (NodeId, u16)) {
        for entry in self.store.iter_mut() {
            if (entry.origin, entry.msg_id) == key {
                entry.pending = false;
            }
        }
    }

    fn store_text(
        &mut self,
        now_us: u64,
        key: (NodeId, u16),
        timestamp: Option<u64>,
        body: &heapless::Vec<u8, { wire::TEXT_CAP }>,
    ) {
        if !self.store_enabled {
            return;
        }
        self.prune_store(now_us);
        if self.store.is_full() {
            self.store.pop_front();
        }
        let _ = self.store.push_back(Stored {
            origin: key.0,
            msg_id: key.1,
            timestamp,
            body: body.clone(),
            expires_at_us: now_us + STORE_TTL_US,
            pending: false,
        });
    }

    /// Entries are in expiry order (insertion order, and restored snapshots
    /// carry less lifetime than fresh entries), so aging out pops the front.
    fn prune_store(&mut self, now_us: u64) {
        while let Some(front) = self.store.front() {
            if front.expires_at_us > now_us {
                break;
            }
            self.store.pop_front();
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
    /// Stamped with UTC when the timeline is GPS-anchored. The queue is
    /// shared with forwards; entries free when
    /// [`on_transmitted`](Self::on_transmitted) confirms they went on the air.
    pub fn queue_text(&mut self, now_us: u64, body: &[u8]) -> Result<(), QueueError> {
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
        // clamped to the u32 range the framing probe assumed (fine to 2106)
        let timestamp = self
            .sync
            .utc_seconds(now_us)
            .map(|utc| utc.min(u64::from(u32::MAX)));
        self.store_text(now_us, (self.node_id, msg_id), timestamp, &queued);
        let _ = self.outbox.push_back(Outgoing {
            origin: self.node_id,
            msg_id,
            hops: 0,
            kind: OutKind::Text {
                timestamp,
                body: queued,
            },
        });
        Ok(())
    }

    /// Set this node's display name and flood the claim (repeated every 10
    /// minutes for late joiners). Purely cosmetic: the node id remains the
    /// protocol identity, and displays should keep showing (part of) the id
    /// alongside the name, since claims are unauthenticated.
    pub fn set_alias(&mut self, now_us: u64, name: &[u8]) -> Result<(), QueueError> {
        if name.len() > wire::ALIAS_CAP {
            return Err(QueueError::TooLong {
                len: name.len(),
                max: wire::ALIAS_CAP,
            });
        }
        if self.outbox.is_full() {
            return Err(QueueError::Busy);
        }
        let mut alias = heapless::Vec::new();
        // bounded by ALIAS_CAP above, so this cannot overflow
        let _ = alias.extend_from_slice(name);
        self.alias = Some(alias.clone());
        self.alias_next_us = now_us + ALIAS_REANNOUNCE_US;
        self.queue_alias_announce(alias);
        Ok(())
    }

    /// The display name `id` has claimed, if one has been heard.
    pub fn alias_of(&self, id: NodeId) -> Option<&[u8]> {
        self.aliases.get(&id).map(|name| name.as_slice())
    }

    /// This node's own display name, if set.
    pub fn alias(&self) -> Option<&[u8]> {
        self.alias.as_deref()
    }

    /// A store node retains delivered (and own) texts and replays them to
    /// [`wire::Recap`] requests. Enable on always-on nodes only; storage is
    /// RAM, so a reboot starts empty (refilled passively from live traffic
    /// and other store nodes' replays).
    pub fn enable_store(&mut self) {
        self.store_enabled = true;
    }

    /// Texts currently retained for replay, for status displays.
    pub fn store_len(&self) -> usize {
        self.store.len()
    }

    /// Serialize the replay store for persistence across reboots (postcard).
    /// Save it whenever [`store_len`](Self::store_len) changes; size `buf`
    /// for `STORE_CAP` entries of `TEXT_CAP` bodies plus framing (8 KiB is
    /// comfortable). Returns `None` when the buffer is too small.
    pub fn store_snapshot<'a>(&self, now_us: u64, buf: &'a mut [u8]) -> Option<&'a [u8]> {
        let mut entries: heapless::Vec<PersistEntry, STORE_CAP> = heapless::Vec::new();
        for stored in &self.store {
            let remaining_us = stored.expires_at_us.saturating_sub(now_us);
            if remaining_us == 0 {
                continue;
            }
            let _ = entries.push(PersistEntry {
                origin: stored.origin,
                msg_id: stored.msg_id,
                timestamp: stored.timestamp,
                remaining_us,
                body: stored.body.clone(),
            });
        }
        postcard::to_slice(&entries, buf).ok().map(|slice| &*slice)
    }

    /// Load a [`store_snapshot`](Self::store_snapshot) from a previous boot,
    /// rebasing each entry's remaining lifetime onto this boot's clock.
    /// Restored keys are marked seen, so live duplicates of restored history
    /// stay deduplicated. Unparseable bytes are ignored.
    pub fn store_restore(&mut self, now_us: u64, bytes: &[u8]) {
        let Ok(entries) = postcard::from_bytes::<heapless::Vec<PersistEntry, STORE_CAP>>(bytes)
        else {
            return;
        };
        for entry in entries {
            if entry.remaining_us == 0 {
                continue;
            }
            let key = (entry.origin, entry.msg_id);
            if self
                .store
                .iter()
                .any(|stored| (stored.origin, stored.msg_id) == key)
            {
                continue;
            }
            self.mark_seen(key);
            if self.store.is_full() {
                self.store.pop_front();
            }
            let _ = self.store.push_back(Stored {
                origin: entry.origin,
                msg_id: entry.msg_id,
                timestamp: entry.timestamp,
                body: entry.body,
                expires_at_us: now_us + entry.remaining_us.min(STORE_TTL_US),
                pending: false,
            });
        }
    }

    /// Broadcast a history request from the next data slot: every store node
    /// in range replays what it has, and dedup discards what this node
    /// already saw. Call on (re)joining the mesh.
    pub fn request_recap(&mut self) {
        self.recap_sends_left = RECAP_SENDS;
    }

    /// Confirm the packet from the most recent [`Action::Transmit`] went on
    /// the air. Without this a queued text is rebuilt into the next data
    /// slot rather than released.
    pub fn on_transmitted(&mut self) {
        match self.tx_carries {
            TxCarries::Nothing => {}
            TxCarries::QueuedText => {
                // our own transmission of this message also covers any
                // pending replay of it
                if let Some(sent) = self.outbox.pop_front() {
                    self.clear_pending((sent.origin, sent.msg_id));
                }
            }
            TxCarries::Replay(origin, msg_id) => self.clear_pending((origin, msg_id)),
            TxCarries::RecapRequest => {
                self.recap_sends_left = self.recap_sends_left.saturating_sub(1)
            }
        }
        self.tx_carries = TxCarries::Nothing;
    }

    /// Next delivered text, oldest first. Flood deduplication already
    /// happened: each message is delivered once no matter how many relayed
    /// or replayed copies arrive.
    pub fn take_text(&mut self) -> Option<Incoming> {
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
        let mut synced_since = *self.synced_since.get_or_insert(position.frame_number);
        // a root change can step the timeline to smaller frame numbers; a
        // listen-window origin from the old timeline would then gag this
        // node's data slots until the new count catches up
        if position.frame_number < synced_since {
            synced_since = position.frame_number;
            self.synced_since = Some(synced_since);
        }
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
                    Payload::Hello => self.build_data(at_us),
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

    /// Direct peers heard within the last 4 frames (any packet with an
    /// embedded hello counts). Every slot holder transmits at least once per
    /// frame, so this drops to 0 within ~16 s of walking out of range.
    pub fn peer_count(&self, now_us: u64) -> usize {
        self.coloring.neighbors_heard(now_us)
    }

    /// Everything known about the neighborhood: direct peers with link
    /// quality and last-heard age, then gossip-only peers attributed to the
    /// neighbor that named them.
    pub fn peers(&self, now_us: u64) -> heapless::Vec<super::PeerInfo, { super::PEER_ROWS }> {
        self.coloring.peers(now_us)
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
        self.tx_carries = TxCarries::Nothing;
        match wire::encode(&wire::Message::Beacon(beacon), &mut self.tx_buf) {
            Ok(packet) => {
                self.tx_len = packet.len();
                true
            }
            Err(_) => false,
        }
    }

    /// The next replayable store entry, once the staggered start has been
    /// reached (oldest first, so a recap arrives in chronological order),
    /// shaped as the outgoing it would be — pinned at the hop cap: delivered,
    /// never re-flooded.
    fn next_replay(&mut self, at_us: u64) -> Option<Outgoing> {
        if at_us < self.replay_start_us {
            return None;
        }
        self.prune_store(at_us);
        self.store
            .iter()
            .find(|entry| entry.pending)
            .map(|entry| Outgoing {
                origin: entry.origin,
                msg_id: entry.msg_id,
                hops: MAX_TEXT_HOPS,
                kind: OutKind::Text {
                    timestamp: entry.timestamp,
                    body: entry.body.clone(),
                },
            })
    }

    /// Build this node's data-slot packet, by priority: a pending recap
    /// request, then queued/forwarded texts and alias announcements, then
    /// recap replays, then a bare hello. Everything embeds the hello, so the
    /// slot claim stays fresh regardless.
    fn build_data(&mut self, at_us: u64) -> bool {
        // standing recap heartbeat, armed on the first data slot so it never
        // preempts the join-time request already in flight
        if self.recap_next_us == 0 {
            self.recap_next_us = at_us + RECAP_PERIOD_US;
        } else if at_us >= self.recap_next_us {
            self.recap_next_us = at_us + RECAP_PERIOD_US;
            self.recap_sends_left = self.recap_sends_left.max(RECAP_SENDS);
        }
        // due re-announce: queue our alias as a fresh flood so late joiners
        // learn it; rides the slot a bare hello would have taken
        if let Some(name) = &self.alias
            && at_us >= self.alias_next_us
            && !self.outbox.is_full()
        {
            let name = name.clone();
            self.alias_next_us = at_us + ALIAS_REANNOUNCE_US;
            self.queue_alias_announce(name);
        }
        let mut hello = self.coloring.hello();
        let (outgoing, carries) = if self.recap_sends_left > 0 {
            (None, TxCarries::RecapRequest)
        } else if let Some(out) = self.outbox.front() {
            (Some(out.clone()), TxCarries::QueuedText)
        } else if let Some(replay) = self.next_replay(at_us) {
            let carries = TxCarries::Replay(replay.origin, replay.msg_id);
            (Some(replay), carries)
        } else {
            (None, TxCarries::Nothing)
        };
        loop {
            let message = match (&outgoing, carries) {
                (_, TxCarries::RecapRequest) => wire::Message::Recap(wire::Recap {
                    hello: hello.clone(),
                }),
                (Some(out), _) => out.to_message(hello.clone()),
                (None, _) => wire::Message::Hello(hello.clone()),
            };
            if let Ok(packet) = wire::encode(&message, &mut self.tx_buf)
                && packet.len() <= usize::from(self.max_packet_len)
            {
                self.tx_len = packet.len();
                self.tx_carries = carries;
                return true;
            }
            if hello.neighbors.pop().is_none() {
                return false;
            }
        }
    }

    fn queue_alias_announce(&mut self, name: heapless::Vec<u8, { wire::ALIAS_CAP }>) {
        let msg_id = self.next_msg_id;
        self.next_msg_id = self.next_msg_id.wrapping_add(1);
        self.mark_seen((self.node_id, msg_id));
        let _ = self.outbox.push_back(Outgoing {
            origin: self.node_id,
            msg_id,
            hops: 0,
            kind: OutKind::Alias { name },
        });
    }
}

impl Outgoing {
    /// The wire form of this outgoing, embedding `hello` (the caller retries
    /// with a smaller hello until the packet fits the slot budget).
    fn to_message(&self, hello: Hello) -> wire::Message {
        match &self.kind {
            OutKind::Text { timestamp, body } => wire::Message::Text(wire::Text {
                hello,
                origin: self.origin,
                msg_id: self.msg_id,
                hops: self.hops,
                timestamp: *timestamp,
                body: body.clone(),
            }),
            OutKind::Alias { name } => wire::Message::Alias(wire::Alias {
                hello,
                origin: self.origin,
                msg_id: self.msg_id,
                hops: self.hops,
                name: name.clone(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tdma::{SlotKind, test_config, test_modulation};

    const FRAME_US: u64 = 16_000_000;

    fn engine(id: u32, seed: u64) -> Engine {
        Engine::new(test_config(), test_modulation(), NodeId(id), seed).unwrap()
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
            Engine::new(config, test_modulation(), NodeId(1), 7),
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
                        + test_modulation()
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
        assert_eq!(a.max_text_len(), 48);
        assert_eq!(
            a.queue_text(5_000_000, &[0u8; 64]),
            Err(QueueError::TooLong { len: 64, max: 48 })
        );
        a.queue_text(5_000_000, b"hi from a").unwrap();
        a.queue_text(5_000_000, b"two").unwrap();
        a.queue_text(5_000_000, b"three").unwrap();
        a.queue_text(5_000_000, b"four").unwrap();
        assert_eq!(a.queue_text(5_000_000, b"overflow"), Err(QueueError::Busy));

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
        b.on_packet(at_us + 60_000, a.packet(), -60).unwrap();
        let incoming = b.take_text().unwrap();
        assert_eq!(incoming.from, NodeId(1));
        assert_eq!(incoming.utc_seconds, None);
        assert_eq!(incoming.body.as_slice(), b"hi from a");
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

        a.queue_text(5_000_000, b"flood me").unwrap();
        let at_us = step_to_data_tx(&mut a, 20_000_000);
        let mut from_a = [0u8; 255];
        let len_a = a.packet().len();
        from_a[..len_a].copy_from_slice(a.packet());

        // b delivers it once and queues a forward with the hop count bumped
        assert_eq!(
            b.on_packet(20_500_000, &from_a[..len_a], -60),
            Ok(Received::Text {
                origin: NodeId(1),
                hops: 0,
            })
        );
        assert!(b.take_text().is_some());
        assert_eq!(
            b.on_packet(20_600_000, &from_a[..len_a], -60),
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
        c.on_packet(21_500_000, &from_b[..len_b], -60).unwrap();
        assert_eq!(c.take_text().map(|m| m.from), Some(NodeId(1)));

        // the author ignores echoes of its own message
        assert_eq!(
            a.on_packet(at_us + 900_000, &from_b[..len_b], -60),
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
            timestamp: None,
            body,
        });
        let mut buf = [0u8; 255];
        let packet = wire::encode(&capped, &mut buf).unwrap();
        d.on_packet(20_500_000, packet, -60).unwrap();
        assert!(d.take_text().is_some());
        let _ = step_to_data_tx(&mut d, 21_000_000);
        assert!(matches!(
            wire::decode(d.packet()),
            Ok(wire::Message::Hello(_))
        ));
    }

    #[test]
    fn texts_carry_utc_when_gps_anchored() {
        let mut a = engine(1, 7);
        a.queue_text(5_000_000, b"early").unwrap();
        a.on_gps_second(20_000_000, 16_000);
        a.queue_text(21_500_000, b"later").unwrap();

        let at_us = step_to_data_tx(&mut a, 21_600_000);
        assert!(matches!(
            wire::decode(a.packet()),
            Ok(wire::Message::Text(t)) if t.body.as_slice() == b"early" && t.timestamp.is_none()
        ));
        a.on_transmitted();
        let _ = step_to_data_tx(&mut a, at_us + 60_000);
        assert!(matches!(
            wire::decode(a.packet()),
            Ok(wire::Message::Text(t))
                if t.body.as_slice() == b"later" && t.timestamp == Some(16_001)
        ));
    }

    /// craft a recap request from `sender` and encode it into `buf`.
    fn recap_packet(sender: u32, buf: &mut [u8; 255]) -> usize {
        let message = wire::Message::Recap(wire::Recap {
            hello: Hello {
                sender: NodeId(sender),
                slot: Some(11),
                neighbors: heapless::Vec::new(),
            },
        });
        wire::encode(&message, buf).unwrap().len()
    }

    #[test]
    fn store_and_recap_replay() {
        let mut author = engine(1, 7);
        let mut relay = engine(2, 8);
        relay.enable_store();

        // author floods two texts; the relay stores them on delivery
        author.queue_text(5_000_000, b"m1").unwrap();
        author.queue_text(5_000_000, b"m2").unwrap();
        let at_a = step_to_data_tx(&mut author, 20_000_000);
        let mut m1 = [0u8; 255];
        let len1 = author.packet().len();
        m1[..len1].copy_from_slice(author.packet());
        author.on_transmitted();
        let _ = step_to_data_tx(&mut author, at_a + 60_000);
        let mut m2 = [0u8; 255];
        let len2 = author.packet().len();
        m2[..len2].copy_from_slice(author.packet());

        relay.on_packet(20_500_000, &m1[..len1], -60).unwrap();
        relay.on_packet(20_600_000, &m2[..len2], -60).unwrap();
        assert_eq!(relay.store_len(), 2);
        while relay.take_text().is_some() {}

        // sync the relay and drain its own flood-forwards of m1/m2, so the
        // session below observes replays rather than forwards
        let mut at_r = 30_000_000;
        loop {
            at_r = step_to_data_tx(&mut relay, at_r);
            let idle = matches!(wire::decode(relay.packet()), Ok(wire::Message::Hello(_)));
            relay.on_transmitted();
            at_r += 60_000;
            if idle {
                break;
            }
        }
        let mut req = [0u8; 255];
        let req_len = recap_packet(3, &mut req);
        assert_eq!(
            relay.on_packet(at_r + 60_000, &req[..req_len], -60),
            Ok(Received::Recap { from: NodeId(3) })
        );
        // a retransmitted request must not restart the running session (the
        // trailing back-to-hellos assert below would catch re-replays)
        relay
            .on_packet(at_r + 70_000, &req[..req_len], -60)
            .unwrap();

        // after the staggered start, replays come oldest-first, pinned at the
        // hop cap so nobody re-floods yesterday's chat
        let mut now = at_r + 100_000;
        let mut replayed: heapless::Vec<heapless::Vec<u8, { wire::TEXT_CAP }>, 4> =
            heapless::Vec::new();
        for _ in 0..16 {
            let at = step_to_data_tx(&mut relay, now);
            if let Ok(wire::Message::Text(t)) = wire::decode(relay.packet()) {
                assert_eq!(t.hops, MAX_TEXT_HOPS);
                replayed.push(t.body.clone()).unwrap();
            }
            relay.on_transmitted();
            now = at + 60_000;
            if replayed.len() == 2 {
                break;
            }
        }
        assert_eq!(replayed.len(), 2);
        assert_eq!(replayed[0].as_slice(), b"m1");
        assert_eq!(replayed[1].as_slice(), b"m2");

        // session drained: back to bare hellos
        let _ = step_to_data_tx(&mut relay, now);
        assert!(matches!(
            wire::decode(relay.packet()),
            Ok(wire::Message::Hello(_))
        ));
    }

    #[test]
    fn replay_suppressed_by_overheard_copy() {
        let mut author = engine(1, 7);
        let mut relay = engine(2, 8);
        relay.enable_store();

        author.queue_text(5_000_000, b"m1").unwrap();
        author.queue_text(5_000_000, b"m2").unwrap();
        let at_a = step_to_data_tx(&mut author, 20_000_000);
        let mut m1 = [0u8; 255];
        let len1 = author.packet().len();
        m1[..len1].copy_from_slice(author.packet());
        author.on_transmitted();
        let _ = step_to_data_tx(&mut author, at_a + 60_000);
        let mut m2 = [0u8; 255];
        let len2 = author.packet().len();
        m2[..len2].copy_from_slice(author.packet());

        relay.on_packet(20_500_000, &m1[..len1], -60).unwrap();
        relay.on_packet(20_600_000, &m2[..len2], -60).unwrap();

        // drain the relay's own flood-forwards first
        let mut at_r = 30_000_000;
        loop {
            at_r = step_to_data_tx(&mut relay, at_r);
            let idle = matches!(wire::decode(relay.packet()), Ok(wire::Message::Hello(_)));
            relay.on_transmitted();
            at_r += 60_000;
            if idle {
                break;
            }
        }
        let mut req = [0u8; 255];
        let req_len = recap_packet(3, &mut req);
        relay
            .on_packet(at_r + 60_000, &req[..req_len], -60)
            .unwrap();

        // another responder replays m1 first; the relay hears the duplicate
        // and crosses it off, so its own session only sends m2
        relay.on_packet(at_r + 120_000, &m1[..len1], -60).unwrap();
        let mut now = at_r + 150_000;
        for _ in 0..16 {
            let at = step_to_data_tx(&mut relay, now);
            if let Ok(wire::Message::Text(t)) = wire::decode(relay.packet()) {
                assert_eq!(t.body.as_slice(), b"m2");
                return;
            }
            relay.on_transmitted();
            now = at + 60_000;
        }
        panic!("replay of m2 never went out");
    }

    /// A root change restarts frame numbers, so deadlines held as frame
    /// numbers from the old timeline would stall for minutes-to-hours
    /// (observed on hardware as a ~5-minute replay delay). Both the replay
    /// stagger and the listen window must ride out the step.
    #[test]
    fn replay_survives_a_root_change() {
        let mut author = engine(1, 7);
        let mut relay = engine(2, 8);
        relay.enable_store();

        author.queue_text(5_000_000, b"m1").unwrap();
        let _ = step_to_data_tx(&mut author, 20_000_000);
        relay.on_packet(20_500_000, author.packet(), -60).unwrap();

        // relay roots itself via gps (frame numbers ~1900), drains its forward
        let mut at_r = 30_000_000;
        loop {
            at_r = step_to_data_tx(&mut relay, at_r);
            let idle = matches!(wire::decode(relay.packet()), Ok(wire::Message::Hello(_)));
            relay.on_transmitted();
            at_r += 60_000;
            if idle {
                break;
            }
        }

        let mut req = [0u8; 255];
        let req_len = recap_packet(3, &mut req);
        relay.on_packet(at_r, &req[..req_len], -60).unwrap();

        // an outranking root whose timeline restarted near zero takes over
        // between the recap and the replay
        let coup = wire::Message::Beacon(crate::tdma::Beacon {
            root: NodeId(1),
            root_has_gps: true,
            stratum: 0,
            frame_number: 3,
        });
        let mut buf = [0u8; 255];
        let coup_len = wire::encode(&coup, &mut buf).unwrap().len();
        relay
            .on_packet(at_r + 100_000, &buf[..coup_len], -60)
            .unwrap();

        // the replay still arrives within the stagger window on the new
        // timeline rather than stalling behind the old frame numbers
        let mut now = at_r + 200_000;
        for _ in 0..12 {
            let at = step_to_data_tx(&mut relay, now);
            if let Ok(wire::Message::Text(t)) = wire::decode(relay.packet()) {
                assert_eq!(t.body.as_slice(), b"m1");
                assert_eq!(t.hops, MAX_TEXT_HOPS);
                return;
            }
            relay.on_transmitted();
            now = at + 60_000;
        }
        panic!("replay stalled after the root change");
    }

    #[test]
    fn store_snapshot_survives_a_reboot() {
        let mut author = engine(1, 7);
        let mut relay = engine(2, 8);
        relay.enable_store();

        author.queue_text(5_000_000, b"keep me").unwrap();
        let _ = step_to_data_tx(&mut author, 20_000_000);
        relay.on_packet(20_500_000, author.packet(), -60).unwrap();
        assert_eq!(relay.store_len(), 1);

        let mut buf = [0u8; 8192];
        let mut snap = [0u8; 8192];
        let len = relay.store_snapshot(21_000_000, &mut buf).unwrap().len();
        snap[..len].copy_from_slice(&buf[..len]);

        // "reboot": a fresh engine with a young clock restores the snapshot
        let mut reborn = engine(2, 8);
        reborn.enable_store();
        reborn.store_restore(1_000_000, &snap[..len]);
        assert_eq!(reborn.store_len(), 1);

        // restored keys stay deduplicated against live copies
        assert!(matches!(
            reborn.on_packet(1_200_000, author.packet(), -60),
            Ok(Received::DuplicateText { .. })
        ));
        assert_eq!(reborn.store_len(), 1);

        // and the restored entry replays to a recap
        let mut at_r = 30_000_000;
        loop {
            at_r = step_to_data_tx(&mut reborn, at_r);
            let idle = matches!(wire::decode(reborn.packet()), Ok(wire::Message::Hello(_)));
            reborn.on_transmitted();
            at_r += 60_000;
            if idle {
                break;
            }
        }
        let mut req = [0u8; 255];
        let req_len = recap_packet(3, &mut req);
        reborn.on_packet(at_r, &req[..req_len], -60).unwrap();
        let mut now = at_r + 60_000;
        for _ in 0..12 {
            let at = step_to_data_tx(&mut reborn, now);
            if let Ok(wire::Message::Text(t)) = wire::decode(reborn.packet()) {
                assert_eq!(t.body.as_slice(), b"keep me");
                return;
            }
            reborn.on_transmitted();
            now = at + 60_000;
        }
        panic!("restored entry never replayed");
    }

    #[test]
    fn restored_entries_keep_only_remaining_ttl() {
        let mut author = engine(1, 7);
        let mut relay = engine(2, 8);
        relay.enable_store();

        author.queue_text(5_000_000, b"old news").unwrap();
        let _ = step_to_data_tx(&mut author, 20_000_000);
        relay.on_packet(20_500_000, author.packet(), -60).unwrap();

        // snapshot taken with one second of lifetime left
        let mut buf = [0u8; 8192];
        let near_expiry = 20_500_000 + STORE_TTL_US - 1_000_000;
        let len = relay.store_snapshot(near_expiry, &mut buf).unwrap().len();

        let mut reborn = engine(2, 8);
        reborn.enable_store();
        reborn.store_restore(1_000_000, &buf[..len]);
        assert_eq!(reborn.store_len(), 1);
        // a second later it ages out on the new boot's clock, not 24h later
        let mut req = [0u8; 255];
        let req_len = recap_packet(3, &mut req);
        reborn.on_packet(2_100_000, &req[..req_len], -60).unwrap();
        assert_eq!(reborn.store_len(), 0);
    }

    #[test]
    fn aliases_flood_and_are_remembered() {
        let mut a = engine(1, 7);
        let mut b = engine(2, 8);
        let mut c = engine(3, 9);
        assert_eq!(
            a.set_alias(5_000_000, b"a name too long"),
            Err(QueueError::TooLong { len: 15, max: 12 })
        );
        a.set_alias(5_000_000, b"noot").unwrap();

        let at_a = step_to_data_tx(&mut a, 20_000_000);
        let alias = match wire::decode(a.packet()) {
            Ok(wire::Message::Alias(alias)) => alias,
            other => panic!("expected alias, got {other:?}"),
        };
        assert_eq!(alias.origin, NodeId(1));
        assert_eq!(alias.hops, 0);
        assert_eq!(alias.name.as_slice(), b"noot");

        // b learns the name, forwards the claim with the hop bumped
        assert_eq!(
            b.on_packet(at_a + 500_000, a.packet(), -60),
            Ok(Received::Alias { origin: NodeId(1) })
        );
        assert_eq!(b.alias_of(NodeId(1)), Some(b"noot".as_slice()));
        assert_eq!(b.alias_of(NodeId(9)), None);
        assert_eq!(
            b.on_packet(at_a + 600_000, a.packet(), -60),
            Ok(Received::DuplicateAlias { origin: NodeId(1) })
        );

        let at_b = step_to_data_tx(&mut b, 21_000_000);
        let fwd = match wire::decode(b.packet()) {
            Ok(wire::Message::Alias(alias)) => alias,
            other => panic!("expected alias forward, got {other:?}"),
        };
        assert_eq!(fwd.hops, 1);
        assert_eq!(fwd.hello.sender, NodeId(2));
        b.on_transmitted();

        // c learns it from the forward, attributed to the author
        let mut copy = [0u8; 255];
        let len = {
            let mut buf = [0u8; 255];
            let p = wire::encode(&wire::Message::Alias(fwd), &mut buf).unwrap();
            copy[..p.len()].copy_from_slice(p);
            p.len()
        };
        c.on_packet(at_b + 500_000, &copy[..len], -60).unwrap();
        assert_eq!(c.alias_of(NodeId(1)), Some(b"noot".as_slice()));

        // after the confirmed announce, a's slot goes back to hellos until
        // the 10-minute re-announce comes due
        a.on_transmitted();
        let _ = step_to_data_tx(&mut a, at_a + 500_000);
        assert!(matches!(
            wire::decode(a.packet()),
            Ok(wire::Message::Hello(_))
        ));
    }

    #[test]
    fn peers_lists_direct_and_gossiped() {
        let mut e = engine(1, 7);
        let mut neighbors = heapless::Vec::new();
        neighbors.push((NodeId(30), 12u16)).unwrap();
        let hello = wire::Message::Hello(Hello {
            sender: NodeId(9),
            slot: Some(11),
            neighbors,
        });
        let mut buf = [0u8; 255];
        let len = wire::encode(&hello, &mut buf).unwrap().len();
        e.on_packet(1_000_000, &buf[..len], -87).unwrap();

        let peers = e.peers(1_500_000);
        assert_eq!(peers.len(), 2);
        assert_eq!(peers[0].id, NodeId(9));
        assert_eq!(peers[0].slot, Some(11));
        assert_eq!(peers[0].rssi_dbm, Some(-87));
        assert_eq!(peers[0].heard_age_us, Some(500_000));
        assert_eq!(peers[0].via, None);
        assert_eq!(peers[1].id, NodeId(30));
        assert_eq!(peers[1].slot, Some(12));
        assert_eq!(peers[1].rssi_dbm, None);
        assert_eq!(peers[1].via, Some(NodeId(9)));

        // past the TTL both disappear (the gossiper carried the 2-hop row)
        assert!(e.peers(1_000_000 + 5 * FRAME_US).is_empty());
    }

    #[test]
    fn returning_peer_triggers_recap() {
        let mut e = engine(1, 7);
        let hello = wire::Message::Hello(Hello {
            sender: NodeId(9),
            slot: Some(11),
            neighbors: heapless::Vec::new(),
        });
        let mut buf = [0u8; 255];
        let len = wire::encode(&hello, &mut buf).unwrap().len();

        // first appearance: not a return, no recap beyond the join-time one
        e.request_recap();
        e.on_packet(1_000_000, &buf[..len], -60).unwrap();
        // drain the join-time recap sends
        let mut now = 20_000_000;
        for _ in 0..RECAP_SENDS {
            now = step_to_data_tx(&mut e, now);
            assert!(matches!(
                wire::decode(e.packet()),
                Ok(wire::Message::Recap(_))
            ));
            e.on_transmitted();
            now += 60_000;
        }
        now = step_to_data_tx(&mut e, now);
        assert!(matches!(
            wire::decode(e.packet()),
            Ok(wire::Message::Hello(_))
        ));

        // silence past the neighbor TTL, then the peer reappears: recap
        let gap = now + 5 * FRAME_US;
        e.on_packet(gap, &buf[..len], -60).unwrap();
        let _ = step_to_data_tx(&mut e, gap + 100_000);
        assert!(matches!(
            wire::decode(e.packet()),
            Ok(wire::Message::Recap(_))
        ));
    }

    #[test]
    fn periodic_recap_rearms() {
        let mut e = engine(1, 7);
        // drain join-time recap sends
        let mut now = 20_000_000;
        for _ in 0..RECAP_SENDS {
            now = step_to_data_tx(&mut e, now);
            e.on_transmitted();
            now += 60_000;
        }
        now = step_to_data_tx(&mut e, now);
        assert!(matches!(
            wire::decode(e.packet()),
            Ok(wire::Message::Hello(_))
        ));

        // past the heartbeat period, the data slot carries a request again
        let later = now + RECAP_PERIOD_US + FRAME_US;
        let _ = step_to_data_tx(&mut e, later);
        assert!(matches!(
            wire::decode(e.packet()),
            Ok(wire::Message::Recap(_))
        ));
    }

    #[test]
    fn peer_count_tracks_recently_heard_nodes() {
        let mut e = engine(1, 7);
        assert_eq!(e.peer_count(1_000_000), 0);
        let hello = wire::Message::Hello(Hello {
            sender: NodeId(9),
            slot: Some(11),
            neighbors: heapless::Vec::new(),
        });
        let mut buf = [0u8; 255];
        let len = wire::encode(&hello, &mut buf).unwrap().len();
        e.on_packet(1_000_000, &buf[..len], -60).unwrap();
        assert_eq!(e.peer_count(1_100_000), 1);
        // silent past the 4-frame neighbor TTL: out of range
        assert_eq!(e.peer_count(1_000_000 + 4 * FRAME_US), 1);
        assert_eq!(e.peer_count(1_000_000 + 4 * FRAME_US + 1), 0);
    }

    #[test]
    fn store_ages_out_by_ttl() {
        let mut relay = engine(2, 8);
        relay.enable_store();
        let mut author = engine(1, 7);
        author.queue_text(5_000_000, b"old news").unwrap();
        let _ = step_to_data_tx(&mut author, 20_000_000);
        relay.on_packet(20_500_000, author.packet(), -60).unwrap();
        assert_eq!(relay.store_len(), 1);

        let at_r = step_to_data_tx(&mut relay, 30_000_000);
        relay.on_transmitted();
        let stale = at_r + STORE_TTL_US + 1_000_000;
        let mut req = [0u8; 255];
        let req_len = recap_packet(3, &mut req);
        relay.on_packet(stale, &req[..req_len], -60).unwrap();
        assert_eq!(relay.store_len(), 0);
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
        let modulation = test_modulation();
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
            receiver.on_packet(t + airtime, &copy[..len], -60).unwrap();
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
        let modulation = test_modulation();
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
            receiver.on_packet(t + airtime, &copy[..len], -60).unwrap();
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
        let modulation = test_modulation();
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
            receiver.on_packet(t + airtime, &copy[..len], -60).unwrap();
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
