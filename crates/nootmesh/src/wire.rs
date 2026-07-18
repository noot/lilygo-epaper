//! On-air packet format for every nootmesh message type.
//!
//! A packet is one plaintext version byte, a 12-byte nonce, and a
//! postcard-encoded [`Message`] sealed with chacha20-poly1305:
//!
//! ```text
//! | version (1 byte) | nonce (12 bytes) | ciphertext | tag (16 bytes) |
//! ```
//!
//! Every packet is encrypted and authenticated with one symmetric key shared
//! by the whole meshnet (baked into each device's firmware at build time), so
//! outsiders can neither read traffic nor inject or tamper with it. The
//! version byte stays plaintext — it is the AEAD associated data, so it is
//! authenticated but readable before decryption.
//!
//! Nonces are `node id (LE) || counter (LE)`. Node ids are unique across the
//! fleet (derived from the eFuse MAC) and the counter starts at per-boot
//! entropy, so nonces never repeat under the shared key — the AEAD's one
//! hard requirement. Relays re-encode (and thereby re-encrypt with a fresh
//! nonce) every message they forward.
//!
//! Authentication is per-packet, not per-author: any key holder can claim
//! any origin, and a replayed packet still authenticates (the engine's
//! `(origin, msg_id)` dedup absorbs replays only while the pair sits in the
//! seen cache). Author identity stays an unauthenticated claim until the
//! pubkey identity layer lands.
//!
//! Postcard's wire format is stable, so compatibility rests on two rules:
//! bump [`VERSION`] on any change to an existing variant's layout, to the
//! envelope, or to the cipher, and only ever *append* variants to
//! [`Message`] (the enum discriminant is a varint of the variant index;
//! appending keeps old packets decoding identically, and old firmware
//! cleanly rejects unknown new discriminants).

use chacha20poly1305::{AeadInPlace as _, ChaCha20Poly1305, Key, KeyInit as _, Nonce, Tag};

use crate::{
    NodeId,
    tdma::{Beacon, Hello},
};

pub const VERSION: u8 = 3;

const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;

/// Bytes every packet carries beyond the postcard body: version, nonce, tag.
pub(crate) const OVERHEAD: usize = 1 + NONCE_LEN + TAG_LEN;

/// Largest packet the radio can carry (the SX126x buffer is 255 bytes).
pub(crate) const MAX_PACKET: usize = 255;

/// Wire capacity for a [`Text`] body. Runtime limits are tighter: the whole
/// packet must fit the slot's airtime budget (see
/// [`Engine::max_text_len`](crate::tdma::Engine::max_text_len)), and staying
/// under 128 keeps postcard's length prefix to one byte.
pub const TEXT_CAP: usize = 127;

/// Display-name length cap: enough to be personal, small enough that alias
/// announcements stay near hello-sized.
pub const ALIAS_CAP: usize = 12;

/// The meshnet-wide AEAD state: the shared key plus this node's nonce
/// counter. One per node; [`encode`] burns one nonce per packet,
/// [`try_decode`] burns none.
pub struct Cipher {
    aead: ChaCha20Poly1305,
    node_id: NodeId,
    counter: u64,
}

impl Cipher {
    /// `key` is the fleet-wide secret; every node must hold the same one.
    /// `counter_seed` must be fresh boot entropy: nonce uniqueness across
    /// this node's reboots rests on two boots' counter ranges not
    /// overlapping in the 2^64 space (uniqueness across *nodes* is
    /// structural — the node id prefixes every nonce).
    pub fn new(key: &[u8; 32], node_id: NodeId, counter_seed: u64) -> Self {
        Self {
            aead: ChaCha20Poly1305::new(Key::from_slice(key)),
            node_id,
            counter: counter_seed,
        }
    }

    fn next_nonce(&mut self) -> [u8; NONCE_LEN] {
        let mut nonce = [0u8; NONCE_LEN];
        nonce[..4].copy_from_slice(&self.node_id.0.to_le_bytes());
        nonce[4..].copy_from_slice(&self.counter.to_le_bytes());
        self.counter = self.counter.wrapping_add(1);
        nonce
    }
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("failed encoding message (buffer too small?): {0}")]
    Encode(postcard::Error),
    #[error("failed decoding packet body: {0}")]
    Decode(postcard::Error),
    #[error("empty packet")]
    Empty,
    #[error("unsupported protocol version {0}, expected {VERSION}")]
    Version(u8),
    #[error("packet length {0} outside the aead envelope bounds")]
    Length(usize),
    #[error("packet failed authentication (wrong mesh key or corrupted)")]
    Auth,
    #[error("encrypting the packet failed")]
    Encrypt,
}

/// Encode and encrypt `message` into `buf`, returning the packet bytes to
/// transmit. Burns one of `cipher`'s nonces.
pub fn encode<'a>(
    message: &Message,
    cipher: &mut Cipher,
    buf: &'a mut [u8],
) -> Result<&'a [u8], Error> {
    let Some((header, rest)) = buf.split_at_mut_checked(1 + NONCE_LEN) else {
        return Err(Error::Encode(postcard::Error::SerializeBufferFull));
    };
    let nonce = cipher.next_nonce();
    header[0] = VERSION;
    header[1..].copy_from_slice(&nonce);
    // reserve the trailing tag space up front so an oversized message
    // surfaces as an encode error instead of clobbering the tag
    let body_cap = rest.len().saturating_sub(TAG_LEN);
    let body_len = postcard::to_slice(message, &mut rest[..body_cap])
        .map_err(Error::Encode)?
        .len();
    let (body, tail) = rest.split_at_mut(body_len);
    let tag = cipher
        .aead
        .encrypt_in_place_detached(Nonce::from_slice(&nonce), &[VERSION], body)
        // only fails past ~2^38 plaintext bytes, unreachable at radio sizes
        .map_err(|_| Error::Encrypt)?;
    tail[..TAG_LEN].copy_from_slice(&tag);
    Ok(&buf[..OVERHEAD + body_len])
}

/// Decrypt and decode a received packet. Never burns a nonce, so log-side
/// peeks at a packet don't disturb the counter.
pub fn try_decode(bytes: &[u8], cipher: &Cipher) -> Result<Message, Error> {
    let (version, rest) = bytes.split_first().ok_or(Error::Empty)?;
    if *version != VERSION {
        return Err(Error::Version(*version));
    }
    if rest.len() < NONCE_LEN + TAG_LEN || bytes.len() > MAX_PACKET {
        return Err(Error::Length(bytes.len()));
    }
    let (nonce, rest) = rest.split_at(NONCE_LEN);
    let (ciphertext, tag) = rest.split_at(rest.len() - TAG_LEN);
    // decrypt into a stack scratch so callers keep handing in `&[u8]`
    let mut scratch = [0u8; MAX_PACKET];
    let body = &mut scratch[..ciphertext.len()];
    body.copy_from_slice(ciphertext);
    cipher
        .aead
        .decrypt_in_place_detached(
            Nonce::from_slice(nonce),
            &[VERSION],
            body,
            Tag::from_slice(tag),
        )
        .map_err(|_| Error::Auth)?;
    postcard::from_bytes(body).map_err(Error::Decode)
}

/// Parse a 64-hex-char string into the 32-byte mesh key; const so the key
/// can be baked in at build time from an env string.
pub const fn key_from_hex(s: &str) -> Option<[u8; 32]> {
    let bytes = s.as_bytes();
    if bytes.len() != 64 {
        return None;
    }
    let mut key = [0u8; 32];
    let mut i = 0;
    while i < 32 {
        let hi = match hex_val(bytes[2 * i]) {
            Some(v) => v,
            None => return None,
        };
        let lo = match hex_val(bytes[2 * i + 1]) {
            Some(v) => v,
            None => return None,
        };
        key[i] = hi << 4 | lo;
        i += 1;
    }
    Some(key)
}

const fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Every message type that can appear on the air. Append-only; see the module
/// docs.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Message {
    Beacon(Beacon),
    Hello(Hello),
    Text(Text),
    Recap(Recap),
    Alias(Alias),
    Position(Position),
}

/// Chat text broadcast in a data slot and flooded across the mesh: every
/// node re-broadcasts each text it has not seen before (once, in its own data
/// slot) until the hop cap. Embeds the transmitter's [`Hello`] so a frame
/// that carries text still refreshes its slot claim in every receiver's
/// neighbor table.
///
/// `hello.sender` is whoever transmitted *this* packet (author or relay);
/// `origin` is the author. `(origin, msg_id)` identifies the message for
/// deduplication, and `hops` counts relays so far (0 = straight from the
/// author). `timestamp` is UTC seconds at origination when the author's
/// timeline was GPS-anchored (`None` on free-running meshes); it rides
/// unchanged through forwards and store-node replays.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Text {
    pub hello: Hello,
    pub origin: NodeId,
    pub msg_id: u16,
    pub hops: u8,
    pub timestamp: Option<u64>,
    pub body: heapless::Vec<u8, TEXT_CAP>,
}

/// Anycast request for stored history: every store node in range replays its
/// retained texts (as ordinary [`Text`]s pinned at the hop cap, so they are
/// delivered but never re-flooded). Receiver-side dedup reconciles overlap
/// between responders and with messages the requester already has.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Recap {
    pub hello: Hello,
}

/// A user-chosen display name for `origin`, flooded like a text (same
/// `(origin, msg_id)` dedup, same hop cap) on join/change and re-announced
/// slowly for late joiners. Purely cosmetic: the node id stays the protocol
/// identity, and displays should show both, since names are unauthenticated
/// claims until the pubkey identity layer lands.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Alias {
    pub hello: Hello,
    pub origin: NodeId,
    pub msg_id: u16,
    pub hops: u8,
    pub name: heapless::Vec<u8, ALIAS_CAP>,
}

/// A node's GPS position, flooded like a text (same `(origin, msg_id)`
/// dedup, same hop cap). Coordinates are degrees scaled by 10^7 (~1 cm
/// resolution). Encrypted like everything else, but nodes still only
/// announce on an explicit user action or an opt-in periodic setting, never
/// by default.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Position {
    pub hello: Hello,
    pub origin: NodeId,
    pub msg_id: u16,
    pub hops: u8,
    pub lat_e7: i32,
    pub lon_e7: i32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NodeId;

    const KEY: [u8; 32] = [7; 32];

    fn cipher() -> Cipher {
        Cipher::new(&KEY, NodeId(0xAABB_CCDD), 99)
    }

    fn roundtrip(message: Message) -> usize {
        let mut buf = [0u8; 255];
        let mut tx = cipher();
        let packet = encode(&message, &mut tx, &mut buf).unwrap();
        assert_eq!(try_decode(packet, &cipher()).unwrap(), message);
        packet.len()
    }

    #[test]
    fn beacon_roundtrips_small() {
        let len = roundtrip(Message::Beacon(Beacon {
            root: NodeId(0x1234_5678),
            root_has_gps: true,
            stratum: 3,
            frame_number: 100_000_000,
        }));
        assert_eq!(len, 13 + OVERHEAD - 1);
    }

    #[test]
    fn hello_roundtrips() {
        let mut neighbors = heapless::Vec::new();
        for i in 0..8u16 {
            neighbors
                .push((NodeId(u32::from(i) + 300), 10 + i))
                .unwrap();
        }
        let len = roundtrip(Message::Hello(Hello {
            sender: NodeId(77),
            slot: Some(42),
            neighbors,
        }));
        assert_eq!(len, 30 + OVERHEAD - 1);
    }

    #[test]
    fn text_roundtrips() {
        let mut body = heapless::Vec::new();
        body.extend_from_slice(b"hello mesh").unwrap();
        let mut neighbors = heapless::Vec::new();
        neighbors.push((NodeId(3), 20u16)).unwrap();
        let len = roundtrip(Message::Text(Text {
            hello: Hello {
                sender: NodeId(77),
                slot: Some(42),
                neighbors,
            },
            origin: NodeId(5),
            msg_id: 300,
            hops: 1,
            timestamp: Some(1_752_300_000),
            body,
        }));
        // ver 1 + nonce 12 + disc 1 + hello 6 + origin 1 + msg_id 2 + hops 1
        //   + timestamp 1+5 + body 1+10 + tag 16
        assert_eq!(len, 57);
    }

    #[test]
    fn position_roundtrips() {
        let len = roundtrip(Message::Position(Position {
            hello: Hello {
                sender: NodeId(77),
                slot: Some(42),
                neighbors: heapless::Vec::new(),
            },
            origin: NodeId(77),
            msg_id: 300,
            hops: 1,
            lat_e7: 405_231_337,
            lon_e7: -740_059_712,
        }));
        // ver 1 + nonce 12 + disc 1 + hello 4 + origin 1 + msg_id 2 + hops 1
        //   + zigzag varint coords ~5 each + tag 16
        assert_eq!(len, 48);
    }

    #[test]
    fn nonce_is_node_id_then_counter_and_never_repeats() {
        let mut tx = Cipher::new(&KEY, NodeId(0x0102_0304), 500);
        let message = Message::Recap(Recap {
            hello: Hello {
                sender: NodeId(1),
                slot: None,
                neighbors: heapless::Vec::new(),
            },
        });
        let mut buf = [0u8; 255];
        let first: heapless::Vec<u8, 255> =
            heapless::Vec::from_slice(encode(&message, &mut tx, &mut buf).unwrap()).unwrap();
        let second = encode(&message, &mut tx, &mut buf).unwrap();
        assert_eq!(&first[1..5], &0x0102_0304u32.to_le_bytes());
        assert_eq!(&first[5..13], &500u64.to_le_bytes());
        assert_eq!(&second[5..13], &501u64.to_le_bytes());
        assert_ne!(&first[1..13], &second[1..13]);
    }

    #[test]
    fn decoding_burns_no_nonce() {
        let message = Message::Recap(Recap {
            hello: Hello {
                sender: NodeId(1),
                slot: None,
                neighbors: heapless::Vec::new(),
            },
        });
        let mut buf = [0u8; 255];
        let mut tx = cipher();
        let packet = encode(&message, &mut tx, &mut buf).unwrap();
        let counter_after_encode = tx.counter;
        try_decode(packet, &tx).unwrap();
        assert_eq!(tx.counter, counter_after_encode);
    }

    #[test]
    fn tampering_fails_auth() {
        let message = Message::Beacon(Beacon {
            root: NodeId(1),
            root_has_gps: false,
            stratum: 0,
            frame_number: 1,
        });
        let mut buf = [0u8; 255];
        let mut tx = cipher();
        let len = encode(&message, &mut tx, &mut buf).unwrap().len();
        // every byte after the version participates in authentication:
        // nonce, ciphertext, and tag flips must all be rejected
        for i in 1..len {
            let mut tampered: heapless::Vec<u8, 255> =
                heapless::Vec::from_slice(&buf[..len]).unwrap();
            tampered[i] ^= 0x01;
            assert_eq!(
                try_decode(&tampered, &cipher()),
                Err(Error::Auth),
                "byte {i}"
            );
        }
    }

    #[test]
    fn wrong_key_fails_auth() {
        let message = Message::Beacon(Beacon {
            root: NodeId(1),
            root_has_gps: false,
            stratum: 0,
            frame_number: 1,
        });
        let mut buf = [0u8; 255];
        let mut tx = cipher();
        let packet = encode(&message, &mut tx, &mut buf).unwrap();
        let other = Cipher::new(&[8; 32], NodeId(2), 0);
        assert_eq!(try_decode(packet, &other), Err(Error::Auth));
    }

    #[test]
    fn plaintext_never_appears_on_the_wire() {
        let mut body = heapless::Vec::new();
        body.extend_from_slice(b"very secret words").unwrap();
        let message = Message::Text(Text {
            hello: Hello {
                sender: NodeId(77),
                slot: Some(42),
                neighbors: heapless::Vec::new(),
            },
            origin: NodeId(5),
            msg_id: 300,
            hops: 0,
            timestamp: None,
            body,
        });
        let mut buf = [0u8; 255];
        let mut tx = cipher();
        let packet = encode(&message, &mut tx, &mut buf).unwrap();
        assert!(!packet.windows(17).any(|w| w == b"very secret words"));
    }

    #[test]
    fn rejects_bad_packets() {
        let rx = cipher();
        assert_eq!(try_decode(&[], &rx), Err(Error::Empty));
        assert_eq!(try_decode(&[9, 0], &rx), Err(Error::Version(9)));
        assert_eq!(try_decode(&[VERSION], &rx), Err(Error::Length(1)));
        assert_eq!(
            try_decode(&[0u8; OVERHEAD - 1], &rx).unwrap_err(),
            Error::Version(0)
        );
        let oversized = [VERSION; MAX_PACKET + 1];
        assert_eq!(
            try_decode(&oversized, &rx),
            Err(Error::Length(MAX_PACKET + 1))
        );

        let message = Message::Beacon(Beacon {
            root: NodeId(1),
            root_has_gps: false,
            stratum: 0,
            frame_number: 1,
        });
        let mut tiny = [0u8; 3];
        let mut tx = cipher();
        assert!(matches!(
            encode(&message, &mut tx, &mut tiny),
            Err(Error::Encode(_))
        ));
    }

    #[test]
    fn truncated_packet_fails() {
        let mut buf = [0u8; 255];
        let message = Message::Hello(Hello {
            sender: NodeId(1_000_000),
            slot: Some(99),
            neighbors: heapless::Vec::new(),
        });
        let mut tx = cipher();
        let packet = encode(&message, &mut tx, &mut buf).unwrap();
        // losing a tail byte breaks the tag
        assert_eq!(
            try_decode(&packet[..packet.len() - 1], &cipher()),
            Err(Error::Auth)
        );
        // losing the whole body leaves too little for the envelope
        assert_eq!(
            try_decode(&packet[..OVERHEAD - 1], &cipher()),
            Err(Error::Length(OVERHEAD - 1))
        );
    }

    #[test]
    fn key_from_hex_parses() {
        let key = key_from_hex("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1F")
            .unwrap();
        assert_eq!(key[0], 0x00);
        assert_eq!(key[1], 0x01);
        assert_eq!(key[10], 0x0a);
        assert_eq!(key[31], 0x1f);
        assert_eq!(key_from_hex(""), None);
        assert_eq!(key_from_hex("0011"), None);
        assert_eq!(
            key_from_hex("zz0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"),
            None
        );
    }
}
