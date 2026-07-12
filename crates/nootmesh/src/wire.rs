//! On-air packet format for every nootmesh message type.
//!
//! A packet is one version byte followed by a postcard-encoded [`Message`]:
//!
//! ```text
//! | version (1 byte) | postcard(Message) |
//! ```
//!
//! Postcard's wire format is stable, so compatibility rests on two rules:
//! bump [`VERSION`] on any change to an existing variant's layout, and only
//! ever *append* variants to [`Message`] (the enum discriminant is a varint of
//! the variant index; appending keeps old packets decoding identically, and
//! old firmware cleanly rejects unknown new discriminants).

use crate::{
    NodeId,
    tdma::{Beacon, Hello},
};

pub const VERSION: u8 = 2;

/// Wire capacity for a [`Text`] body. Runtime limits are tighter: the whole
/// packet must fit the slot's airtime budget (see
/// [`Engine::max_text_len`](crate::tdma::Engine::max_text_len)), and staying
/// under 128 keeps postcard's length prefix to one byte.
pub const TEXT_CAP: usize = 127;

/// Every message type that can appear on the air. Append-only; see the module
/// docs.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Message {
    Beacon(Beacon),
    Hello(Hello),
    Text(Text),
    Recap(Recap),
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
}

/// Encode `message` into `buf`, returning the packet bytes to transmit.
pub fn encode<'a>(message: &Message, buf: &'a mut [u8]) -> Result<&'a [u8], Error> {
    let (version, body) = buf.split_first_mut().ok_or(Error::Empty)?;
    *version = VERSION;
    let body_len = postcard::to_slice(message, body)
        .map_err(Error::Encode)?
        .len();
    Ok(&buf[..1 + body_len])
}

/// Decode a received packet.
pub fn decode(packet: &[u8]) -> Result<Message, Error> {
    let (version, body) = packet.split_first().ok_or(Error::Empty)?;
    if *version != VERSION {
        return Err(Error::Version(*version));
    }
    postcard::from_bytes(body).map_err(Error::Decode)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NodeId;

    fn roundtrip(message: Message) -> usize {
        let mut buf = [0u8; 255];
        let packet = encode(&message, &mut buf).unwrap();
        assert_eq!(decode(packet).unwrap(), message);
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
        assert_eq!(len, 13);
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
        assert_eq!(len, 30);
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
        // ver 1 + disc 1 + hello 6 + origin 1 + msg_id 2 + hops 1
        //   + timestamp 1+5 + body 1+10
        assert_eq!(len, 29);
    }

    #[test]
    fn rejects_bad_packets() {
        assert_eq!(decode(&[]), Err(Error::Empty));
        assert_eq!(decode(&[9, 0]), Err(Error::Version(9)));
        assert!(matches!(decode(&[VERSION]), Err(Error::Decode(_))));
        assert!(matches!(decode(&[VERSION, 200]), Err(Error::Decode(_))));

        let message = Message::Beacon(Beacon {
            root: NodeId(1),
            root_has_gps: false,
            stratum: 0,
            frame_number: 1,
        });
        let mut tiny = [0u8; 3];
        assert!(matches!(encode(&message, &mut tiny), Err(Error::Encode(_))));
    }

    #[test]
    fn truncated_packet_fails() {
        let mut buf = [0u8; 255];
        let message = Message::Hello(Hello {
            sender: NodeId(1_000_000),
            slot: Some(99),
            neighbors: heapless::Vec::new(),
        });
        let packet = encode(&message, &mut buf).unwrap();
        assert!(matches!(
            decode(&packet[..packet.len() - 1]),
            Err(Error::Decode(_))
        ));
    }
}
