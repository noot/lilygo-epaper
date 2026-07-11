# nootmesh

A simple mesh protocol built on top of LoRa.

details:
- relay node support (determine hop count)
- supports synchronization (TDMA) to coordinate transmittions over the same channel
- message storage and redelivery in case of non-receipt (ack messages, or have relays store x most recent messages and users pull on demand?)
- message "to:user" and "to:all" options (nodes simply don't display messages that aren't to them)
- pubkey identities for remote peer verification
- message encryption (optionally, can be required per-user) handshake to determine shared key (dh? noise protocol to prove remote actually has the key?)
- routing/peer discovery


## tdma (implemented in `crates/nootmesh/src/tdma`)

Sized for the sx1262 defaults (915 MHz, SF7, 125 kHz, CR 4/5): a 64-byte
payload flies in 118 ms, so slots are 160 ms with a 15 ms guard on each edge.
100 slots per frame = 16 s frames, anchored at `utc % 16 == 0` (~0.7% duty
cycle per slot held).

Frame layout: `| 0..4 beacon | 4..10 contention (hellos) | 10..100 data |`

Time sync: one elected root (GPS fix beats none, then lowest id) anchors the
frame to UTC and floods it via beacons; stratum-k relays rebeacon in slot
`min(k, 3)`, receivers recover the origin from RxDone − airtime − guard.
Because the T5S3 GPS is NMEA-only (no PPS wired), the root free-runs on its
crystal and only re-anchors on gross (>200 ms) disagreement, so NMEA jitter
never steps the mesh timeline — GPS supplies frame *numbering*, beacons supply
edge timing.

Data slots: distributed 2-hop greedy coloring, first pick seeded by
`fnv1a(node_id)` so simultaneous cold boots spread out, conflicts resolved
lower-id-wins.

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
