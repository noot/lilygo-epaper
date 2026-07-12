# nootmesh

A simple mesh protocol built on top of LoRa.

details:
- relay node support w max hops and msg dedup
- supports synchronization (TDMA) to coordinate transmittions over the same channel
- message storage and redelivery in case of non-receipt (have relays store x most recent messages and users pull on demand)

todo:
- store received messages on sd card for persistence (long time period on t5s3, smaller window on t3s3 relays)
- message "to:user" and "to:all" options (nodes simply don't display messages that aren't to them)
- pubkey identities for remote peer verification
- message encryption (optionally, can be required per-user) handshake to determine shared key (dh? noise protocol to prove remote actually has the key?)
- routing/peer discovery (gossip flood that says "who's in the mesh", useful for determining topology)

## tdma (implemented in `crates/nootmesh/src/tdma`)

The fleet profile is the single point of truth: `nootmesh::tdma::Config::default()`
+ `airtime::Modulation::default()` — every firmware target builds both its
engine and its radio-driver config from them (mismatched nodes cannot sync,
so nothing else may hardcode modulation or frame layout).

Current profile (915 MHz, SF7, 125 kHz, CR 4/5): 200 ms slots fit a 99-byte
payload (169 ms) inside 15 ms guards; 20 slots = 4 s frames anchored at
`utc % 4 == 0`. Texts up to 91 bytes. Sized for a small (~6 node) mesh:
sync, election and delivery settle in seconds. (An earlier 160 ms × 100 =
16 s layout is kept as the unit tests' fixed arithmetic base.)

Frame layout: `| 0..3 beacon | 3..6 contention (hellos) | 6..20 data |`

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
(71 bytes at SF7 defaults).

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
the slot budget (76 bytes at the current profile). Per-slot fragmentation is
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
1–6 seeded frames, and *hearing* any transmission of a message (another
responder's replay, a live flood, or its own forward going out) crosses that
message off its pending session — so the earliest responder does most of the
talking and the others fill gaps. Suppression assumes shared audibility;
hidden-terminal responders degrade to redundant-but-correct.

Known limits: recap is direct-range only (neither request nor replies flood);
storage is RAM, so a rebooted relay starts empty and refills passively from
live traffic and other relays' replays (SD persistence is in the todo);
silence after a recap is ambiguous between "no store node in range" and
"nothing missed".

## wire format (implemented in `crates/nootmesh/src/wire.rs`)

`| version byte | postcard(Message) |` — postcard over protobuf because frames
are Rust-to-Rust (bridges translate for other consumers at the edge), it has a
stable wire spec, and it costs zero field-tag bytes on air. Compatibility
rules: bump the version byte on layout changes to existing variants; only
append variants to the `Message` enum. A beacon is 13 bytes on air.

## message types

```
// can probably start w just dh and move to noise or something later
//
// node sends a handshake; remote also responds with a handshake
struct Handshake {
    public_key: PublicKey,
    alias: String,
    encryption_mode: EncryptionMode
}

enum EncryptionMode {
    Always,
    Optional,
    Plaintext,
}

struct UserMessage {
    to: Option<PublicKey>, // none for broadcast to all
    contents: Bytes,
}


```
