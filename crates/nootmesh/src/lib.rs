//! nootmesh: a simple mesh protocol over LoRa.
//!
//! Pure `no_std` protocol logic with no hardware dependencies: timestamps are
//! `u64` microseconds from any monotonic local clock, and radio I/O is driven
//! by the caller. See [`tdma`] for channel access and time synchronization.

#![no_std]

pub mod airtime;
pub mod tdma;
pub mod wire;

/// Node identity. Will become a public-key fingerprint once identities land;
/// for now a plain integer that must be unique across the mesh.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct NodeId(pub u32);
