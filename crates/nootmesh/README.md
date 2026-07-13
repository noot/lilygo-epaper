# nootmesh

A simple mesh protocol built on top of LoRa.

details:
- relay node support w max hops and msg dedup
- supports synchronization (TDMA) to coordinate transmittions over the same channel
- message storage and redelivery in case of non-receipt (relays store most recent messages and users pull on demand)
- store received messages on sd card for persistence

todo:
- message "to:user" and "to:all" options (nodes simply don't display messages that aren't to them)
- pubkey identities for remote peer verification
- message encryption (optionally, can be required per-user) handshake to determine shared key (dh? noise protocol to prove remote actually has the key?)
- routing/peer discovery (gossip flood that says "who's in the mesh", useful for determining topology)

## tdma (implemented in `crates/nootmesh/src/tdma`)

The fleet profile is the single point of truth: `nootmesh::tdma::Config::default()`
+ `airtime::Modulation::default()` — every firmware target builds both its
engine and its radio-driver config from them (mismatched nodes cannot sync,
so nothing else may hardcode modulation or frame layout).

Current profile (915 MHz, SF9, 125 kHz, CR 4/5): 450 ms slots fit a 71-byte
payload (411 ms) inside 15 ms guards; 20 slots = 9 s frames anchored at
`utc % 9 == 0`. Text bodies up to ~44 chars. Chosen for ~2x the range of SF7
(~+6 dB) at still-tolerable latency; see the modulation ladder below. (An
earlier SF7 / 160 ms x 100 = 16 s layout is kept as the unit tests' fixed
arithmetic base.)

Frame layout: `| 0..3 beacon | 3..6 contention (hellos) | 6..20 data |`

### modulation ladder

What each spreading factor would look like for nootmesh, all at BW 125 kHz /
CR 4/5 with the 20-slot (3 beacon / 3 contention / 14 data) layout, 15 ms
guards, and slots sized so the max packet plus beacon-jitter headroom fits
(slot >= 2*(guard + 16-byte airtime); frames padded to whole seconds).
Link budget and range multipliers are approximate (free-space math; clutter
compresses it). Every step is a two-line profile change
(`Modulation::default` + `Config::default`) and a full-fleet reflash —
mixed spreading factors cannot hear each other at all.

| SF | link vs SF7 | ~range | slot | frame | max packet | max text | per-hop latency |
|---|---|---|---|---|---|---|---|
| 7 | — | ~0.5–1 km | 200 ms | 4 s | 99 B | ~73 ch | ≤4 s |
| 8 | +3 dB | ~1.4x | 300 ms | 6 s | 86 B | ~59 ch | ≤6 s |
| **9 (current)** | **+6 dB** | **~2x** | **450 ms** | **9 s** | **71 B** | **~44 ch** | **≤9 s** |
| 10 | +9 dB | ~2.8x | 900 ms | 18 s | 84 B | ~57 ch | ≤18 s |
| 11 | +11.5 dB | ~3.8x | 1.8 s | 36 s | 76 B | ~49 ch | ≤36 s |
| 12 | +14 dB | ~5x | 3 s | 60 s | 70 B | ~43 ch | ≤60 s |

Everything scales with the frame: root election, recap replays (one message
per frame), sync expiry, listen windows. SF11 at these settings is
Meshtastic-LongFast-class range (their default is SF11/BW250 — same symbol
rate as SF10/BW125 with ~2.5 dB less sensitivity; they tolerate the airtime
because managed-flood CSMA has no slots to size). SF12 is telemetry
territory, not chat. Max packet does not fall monotonically because frames
are padded to whole seconds — some SFs land more slack than others.

Time sync: one elected root (GPS fix beats none, then lowest id) anchors the
frame to UTC and floods it via beacons; stratum-k relays rebeacon in slot
`min(k, beacon_slots-1)`, receivers recover the origin from
RxDone − airtime − guard − jitter. Beacons carry a deterministic per-frame
transmit jitter derived from `(root id, frame number)` — reconstructible by
receivers from the beacon payload — because two GPS roots share the UTC slot
grid by construction and would otherwise collide slot-0-on-slot-0 every frame
and never hear each other to resolve the election (observed on hardware as
"every node says ROOT").
Because the T5S3 GPS is NMEA-only (no PPS wired), the root free-runs on its
crystal and only re-anchors on gross (>200 ms) disagreement, so NMEA jitter
never steps the mesh timeline — GPS supplies frame *numbering*, beacons supply
edge timing.

Data slots: distributed 2-hop greedy coloring, first pick seeded by
`fnv1a(node_id)` so simultaneous cold boots spread out, conflicts resolved
lower-id-wins.

No-GPS meshes (e.g. T3-S3-only): unsynced nodes listen 2-5 frames (plus a
sub-frame jitter, so two simultaneous roots' slot grids can't align and
collide beacon-on-beacon every frame) then self-appoint as a free-running
root. GPS-anchored beacons outrank free-running ones, then lowest id; a root
never expires (it is its own time source) and cedes only to an outranking
beacon.

## root failover

Rank order everywhere: GPS-anchored beats free-running, then lowest id.

When the root dies (power/range), the mesh heals unattended:

1. **Holdover, ~32 s** (`EXPIRY_FRAMES` = 8 quiet frames): beacon-synced
   nodes free-run on their crystals; slots, sync and chat keep working.
   Sized generously so ordinary deafness (e-paper flushes, SD scans) never
   triggers a spurious re-election — faster failover would mean false
   positives.
2. **Expiry**: nodes drop to unsynced and stop transmitting. Chat pauses;
   queued/forwarding texts stay queued (in-flight floods interrupted here may
   never finish their hops — redelivery is future work).
3. **Succession**: a surviving GPS-fixed node re-anchors as root on its next
   parsed NMEA second — seconds after expiry, same UTC grid, timeline barely
   steps. With no GPS survivor, the free-running fallback race runs instead
   (2-5 frames listen + jitter, first to expire self-appoints, lowest id wins
   contests) and frame numbers restart near 0 — the timeline unmoors from UTC
   until a GPS node returns and outranks.
4. **Recovery**: adopters wait the 2-frame listen window, re-claim slots
   (seeded picks usually land everyone back on their old slot), chat resumes.
   Worst case ≈ one minute of silence end to end; seconds when a GPS
   successor exists.

A returning root still believes it is root (roots never expire) — a two-root
contest, which the per-frame beacon jitter makes resolvable: they hear each
other within a few frames and rank decides. If the returnee outranks the
interim root the mesh steps back onto its timeline; either way convergence is
a few frames.

## engine (implemented in `crates/nootmesh/src/tdma/engine.rs`)

`Engine` ties sync + coloring + wire into the loop the firmware drives: feed
`on_packet`/`on_gps_second`, ask `next_action(now)` → `Listen { revisit_us }`
or `Transmit { at_us }` (packet already encoded in `Engine::packet`). Policy:
root beacons every frame, relays skip half their turns (seeded per-frame coin,
stable within a frame), nodes listen 2 frames before their first slot claim,
slot holders hello every frame (TTL keepalive), saturated nodes fall back to
random contention-slot hellos. Hellos are trimmed to the slot's airtime budget
(the max-packet column above).

## chat texts + flooding (wire `Text` + engine outbox/inbox, wire v2)

`Message::Text { hello, origin, msg_id, hops, timestamp, body }` — broadcast
in a data slot instead of the bare hello (embedding it keeps the
transmitter's slot claim fresh). Texts flood: every node re-broadcasts each
unseen message once in its own data slot until `hops` reaches 3, so chat
crosses multi-hop topologies at ≤1 frame per hop. Dedup is `(origin,
msg_id)` — an explicit id rather than a content hash, so identical bodies
sent twice are distinct — in a 32-entry seen-cache that also silences relayed
echoes of one's own messages; delivery to the app is exactly-once via the
inbox. `timestamp` is UTC seconds at origination when the author's timeline
is GPS-anchored (`None` on free-running meshes — frame numbers have no UTC
meaning there); it rides unchanged through forwards and replays, so recapped
messages display their send time, not their arrival time. Outgoing queue
holds 4 (own + forwards; forwards drop silently when full). Body capped to
the slot budget (~44 bytes at the current profile). Per-slot fragmentation is
still future work. The t5s3-epaper-ui lora page is the first consumer (see
`t5s3-epaper-ui/src/mesh.rs` for the servicing-slice pattern that gives the
50 ms ui loop millisecond-precise mesh timing).

## message storage + recap (wire `Recap`, engine store)

Store nodes (always-on relays; `Engine::enable_store()`, on the T3-S3s)
retain delivered and own texts in a RAM ring: last 24 h by the node's own
monotonic clock **or** last 32 messages, whichever runs out first (frame
numbers unmoor across root changes, so they cannot measure age). A node
(re)joining the mesh broadcasts `Recap { hello }` — **anycast**, deliberately
not addressed: LoRa is physically broadcast, redundant responders cover each
other's gaps (a rebooted relay's ring may have holes), and receiver-side
dedup makes overlap harmless.

Replays are ordinary `Text`s pinned at the hop cap — delivered to everyone in
range but never re-flooded — sent oldest-first, one per data slot. Replay-ness
needs no marker: every consumer already does the right thing from fields that
are present (`(origin, msg_id)` for dedup/storage, `hops` for forwarding).

Duplicate-responder suppression: each store node staggers its replay start by
1–6 frames' worth of *local-clock time* (never a frame-number deadline: a
root change restarts frame numbers, which left a frame-based deadline minutes
in the new timeline's future — observed on hardware as a ~5-minute replay
stall; the engine's listen window guards against the same timeline step), and *hearing* any transmission of a message (another
responder's replay, a live flood, or its own forward going out) crosses that
message off its pending session — so the earliest responder does most of the
talking and the others fill gaps. Suppression assumes shared audibility;
hidden-terminal responders degrade to redundant-but-correct.

Recap triggers: mesh join, a *returning peer* (one pruned for silence, now
heard again — either side may hold messages from the time apart, and both
sides' triggers fire, so history flows both ways), and a 30-minute heartbeat
(local clock) that heals gaps no event catches, e.g. a range-degraded link
that never fully dropped. Redundant recaps are cheap: dedup discards
already-held replays and suppression collapses redundant responders.

The request is retransmitted 3 times, one data slot apart: a one-shot request
can vanish into a receiver's display-refresh deafness window (observed on
hardware — the T3-S3 refreshed its panel right after its own hello every
frame, systematically shadowing the same slots each frame; it now refreshes
every 4th frame). Store nodes ignore repeat requests while a replay session
is already running, so retries never restart a session.

The store survives reboots: `Engine::store_snapshot` serializes the ring
(postcard) with retention as *remaining* lifetime — absolute expiry instants
would be meaningless on the next boot's clock — and `store_restore` rebases
them and re-marks the keys seen, so live duplicates of restored history stay
deduplicated. The t3s3 relay saves to SD on every store change and reloads
at boot, making the full 24 h mailbox window hold across power cycles.

Known limits: recap is direct-range only (neither request nor replies flood);
silence after a recap is ambiguous between "no store node in range" and
"nothing missed".

## wire reference (`crates/nootmesh/src/wire.rs`, VERSION 2)

Every packet is:

```
| version (1 byte) | postcard(Message) |
```

postcard over protobuf because frames are Rust-to-Rust (bridges translate for
other consumers at the edge), it has a stable wire spec, and it costs zero
field-tag bytes on air. Compatibility rules: bump the version byte on layout
changes to existing variants; only *append* variants to `Message` (the enum
discriminant is a varint of the variant index, so old packets keep decoding
and old firmware cleanly rejects unknown discriminants). Sizes below are
postcard varints, so small values encode small.

Version history: v0 = Beacon + Hello; v1 = + Text (flood fields); v2 = Text
gained `timestamp` (layout change → bump; Recap was appended in the same
release and alone would not have needed one).

```rust
enum Message {          // discriminant
    Beacon(Beacon),     // 0
    Hello(Hello),       // 1
    Text(Text),         // 2
    Recap(Recap),       // 3
    Alias(Alias),       // 4
}
```

### Beacon — time sync (10–20 B on air)

```rust
Beacon {
    root: NodeId,        // the elected root this timeline belongs to
    root_has_gps: bool,  // rank: GPS-anchored beats free-running
    stratum: u8,         // relay hops from the root (0 = the root itself)
    frame_number: u64,   // the frame this beacon is transmitted in
}
```

Sent every frame by the root in beacon slot 0; stratum-k nodes relay in slot
`min(k, beacon_slots-1)` on a per-frame coin flip, never at stratum ≥ 7.
Transmit time is `slot start + guard + jitter(root, frame_number)` — both
jitter inputs are in the payload, so a receiver recovers the frame origin
from its RxDone timestamp alone: `origin = rx_end − airtime − guard − jitter
− slot·slot_len`. No timestamp field is needed on the wire.

Receiver: adopt if it outranks the current root (GPS > free-running, then
lowest id), or same root at lower stratum (refresh). A root ignores beacons
it outranks — including echoes of its own timeline.

### Hello — presence + slot claim (6 B + ~2–7 B per neighbor)

```rust
Hello {
    sender: NodeId,
    slot: Option<u16>,                        // sender's claimed data slot
    neighbors: Vec<(NodeId, u16), 16>,        // sender's 1-hop table w/ slots
}
```

The data-slot keepalive (sent every frame by slot holders; claims expire from
neighbor tables after 4 quiet frames) and the bootstrap announcement
(contention slots, when every data slot looks taken). The neighbor list gives
receivers the 2-hop visibility the slot coloring needs; it is trimmed
entry-by-entry to fit the slot's airtime budget. Every other data-slot
message *embeds* a Hello for the same reason.

Receiver: upsert the sender in the neighbor table; yield own slot claim if an
outranking (lower-id) claimant appears at 1 or 2 hops.

### Text — chat, flooded (≤ 71 B on air at the current profile)

```rust
Text {
    hello: Hello,           // the *transmitter's* (author or relay)
    origin: NodeId,         // the author, forever
    msg_id: u16,            // author-stamped; (origin, msg_id) is the dedup key
    hops: u8,               // relays so far; 0 = straight from the author
    timestamp: Option<u64>, // UTC seconds at origination (None: not GPS-anchored)
    body: Vec<u8, 127>,     // ~44 B at the current profile (slot budget)
}
```

Sent in the sender's data slot. Floods: each receiver re-broadcasts an unseen
message once from its own data slot while `hops < 3`, giving ≤ 1 frame per
hop. Store-node replays are ordinary Texts pinned at `hops = 3` (delivered,
never re-forwarded) — replay-ness needs no marker. `timestamp` rides
unchanged through forwards and replays.

Receiver: process the embedded hello always; dedup on `(origin, msg_id)`
against the seen-cache (duplicates also cross matching entries off any
pending replay session); deliver exactly once to the inbox; queue a forward
if under the hop cap; store nodes retain it for recap.

### Alias — display-name claim (~12 B + name on air)

```rust
Alias {
    hello: Hello,           // the transmitter's (author or relay)
    origin: NodeId,         // whose name this is
    msg_id: u16,            // (origin, msg_id) dedup, like Text
    hops: u8,
    name: Vec<u8, 12>,
}
```

Flooded like a Text (same dedup, same hop cap) when a user sets their name,
and re-flooded every 10 minutes (local clock) so late joiners learn it — the
re-announce rides a data slot that would have carried a bare hello, so its
steady cost is only the name bytes. Receivers keep a 16-entry id → name
table. Purely cosmetic: node id remains the protocol identity, and displays
show `name (137c)` — name plus id tail — because claims are unauthenticated
until the pubkey layer. Nodes that never announce appear as bare ids.

Receiver: process the hello; dedup; record the claim; forward under the hop
cap.

### Recap — anycast history request (10–20 B on air)

```rust
Recap {
    hello: Hello,   // the requester's; also refreshes its slot claim
}
```

Broadcast from the requester's data slot on (re)joining the mesh,
retransmitted 3 times one slot apart (a one-shot can vanish into a
receiver's display-refresh deafness). Deliberately unaddressed: every store
node in range answers, staggered 1–6 frames with hear-and-skip suppression
between responders; receiver-side dedup reconciles all overlap.

Receiver: process the hello; store nodes with retained texts (and no session
already running) start a replay session, oldest first, one per data slot.

### data-slot content priority

A node's own data slot carries exactly one message per frame, chosen as:
pending recap request > queued/forwarded texts and alias announcements (one
shared queue, FIFO) > recap replay > bare hello. Everything embeds the
hello, so the slot claim never lapses.

### protocol constants (engine defaults)

| constant | value | meaning |
|---|---|---|
| guard | 15 ms | slot-edge margin; in-slot TX starts here |
| beacon jitter range | (slot − 2·guard)/2 | per-frame root decorrelation |
| LISTEN_FRAMES | 2 | sync → first slot claim |
| EXPIRY_FRAMES | 8 | beacon silence → unsynced (roots never expire) |
| root fallback | 2–5 frames + sub-frame jitter | listen before self-rooting |
| MAX_STRATUM | 7 | beacons not adopted/relayed beyond |
| NEIGHBOR_TTL | 4 frames | quiet neighbor's claim forgotten |
| MAX_TEXT_HOPS | 3 | flood radius |
| SEEN_CAP / OUTBOX_CAP | 32 / 4 | dedup window / pending texts |
| STORE_CAP / STORE_TTL | 32 / 24 h | replay mailbox (local-clock TTL) |
| RECAP_SENDS | 3 | request retransmits |
| replay stagger | 1–6 frames (local-clock) | responder desync |

All durations are the node's own monotonic clock — never frame numbers, which
restart when a root changes.

## planned message types (not yet on the wire)

```
// can probably start w just dh and move to noise or something later
//
// node sends a handshake; remote also responds with a handshake
struct Handshake {
    public_key: PublicKey,
    alias: String,
    encryption_mode: EncryptionMode  // Always | Optional | Plaintext
}

// directed messaging: like Text but to: Some(recipient), body encrypted
// under the handshake-derived key; nodes still relay what they can't read
struct UserMessage {
    to: Option<PublicKey>, // none for broadcast to all
    contents: Bytes,
}
```

Both are appends to `Message`, so they will not need a version bump.
