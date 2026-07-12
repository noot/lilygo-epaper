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

use crate::tdma::{Beacon, Hello};

pub const VERSION: u8 = 0;

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
}

/// Chat text broadcast in the sender's data slot. Embeds the sender's
/// [`Hello`] so a frame that carries text still refreshes the sender's slot
/// claim in every receiver's neighbor table.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Text {
    pub hello: Hello,
    pub body: heapless::Vec<u8, TEXT_CAP>,
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
            body,
        }));
        // ver 1 + disc 1 + sender 1 + slot 2 + neighbors 3 + body 1+10
        assert_eq!(len, 19);
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
