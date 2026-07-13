use heapless::{FnvIndexMap, Vec};

use super::Config;
use crate::NodeId;

/// Maximum tracked 1-hop neighbors, and entries in a [`Hello`] neighbor list.
pub const MAX_NEIGHBORS: usize = 16;

/// Frames without hearing a neighbor before its slot claim is forgotten.
const NEIGHBOR_TTL_FRAMES: u64 = 4;

/// Neighbor announcement: the sender's claimed slot plus its own 1-hop table,
/// which gives receivers the 2-hop visibility the coloring needs.
///
/// Nodes without a data slot send hellos in contention slots; once assigned,
/// they piggyback the same information on transmissions in their own slot.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Hello {
    pub sender: NodeId,
    pub slot: Option<u16>,
    pub neighbors: Vec<(NodeId, u16), MAX_NEIGHBORS>,
}

#[derive(Debug)]
struct Neighbor {
    slot: Option<u16>,
    last_heard_us: u64,
    rssi_dbm: i16,
    neighbors: Vec<(NodeId, u16), MAX_NEIGHBORS>,
}

/// A status-display row: a peer this node knows about, directly (heard
/// recently, with link quality) or only via a neighbor's gossip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerInfo {
    pub id: NodeId,
    pub slot: Option<u16>,
    /// microseconds since last heard directly; None for gossip-only peers.
    pub heard_age_us: Option<u64>,
    /// signal strength of the last direct reception.
    pub rssi_dbm: Option<i16>,
    /// the direct neighbor whose hello named this peer (gossip-only peers).
    pub via: Option<NodeId>,
}

/// Capacity of a peer listing: every direct neighbor plus as many
/// gossip-only peers again.
pub const PEER_ROWS: usize = 2 * MAX_NEIGHBORS;

/// Distributed data-slot assignment: greedy graph coloring over the 2-hop
/// neighborhood (2 hops because two transmitters that share no link can still
/// collide at a receiver between them).
///
/// Conflicts are resolved deterministically: the lower id keeps the slot, the
/// higher id re-picks, so simultaneous claims always converge. First picks are
/// seeded by a hash of the node id, so nodes booting together with empty
/// tables still spread across the slot space instead of all grabbing the
/// first data slot.
pub struct Coloring {
    config: Config,
    node_id: NodeId,
    my_slot: Option<u16>,
    neighbors: FnvIndexMap<NodeId, Neighbor, MAX_NEIGHBORS>,
    /// peers pruned for silence, remembered so their return is detectable
    /// (a returning peer may hold messages exchanged while apart).
    lost: Vec<NodeId, MAX_NEIGHBORS>,
}

impl Coloring {
    pub fn new(config: Config, node_id: NodeId) -> Self {
        Self {
            config,
            node_id,
            my_slot: None,
            neighbors: FnvIndexMap::new(),
            lost: Vec::new(),
        }
    }

    pub fn slot(&self) -> Option<u16> {
        self.my_slot
    }

    /// Everything known about the neighborhood, for status displays: direct
    /// peers (within the TTL, freshest first is NOT guaranteed — table
    /// order), then peers known only from neighbors' hellos, attributed to
    /// the gossiping neighbor.
    pub fn peers(&self, now_us: u64) -> Vec<PeerInfo, PEER_ROWS> {
        let ttl = NEIGHBOR_TTL_FRAMES * self.config.frame_us();
        let mut rows: Vec<PeerInfo, PEER_ROWS> = Vec::new();
        for (id, neighbor) in &self.neighbors {
            let age = now_us.saturating_sub(neighbor.last_heard_us);
            if age > ttl {
                continue;
            }
            let _ = rows.push(PeerInfo {
                id: *id,
                slot: neighbor.slot,
                heard_age_us: Some(age),
                rssi_dbm: Some(neighbor.rssi_dbm),
                via: None,
            });
        }
        for (via, neighbor) in &self.neighbors {
            if now_us.saturating_sub(neighbor.last_heard_us) > ttl {
                continue;
            }
            for (id, slot) in &neighbor.neighbors {
                if *id == self.node_id || rows.iter().any(|row| row.id == *id) {
                    continue;
                }
                let _ = rows.push(PeerInfo {
                    id: *id,
                    slot: Some(*slot),
                    heard_age_us: None,
                    rssi_dbm: None,
                    via: Some(*via),
                });
            }
        }
        rows
    }

    /// Direct peers heard within the neighbor TTL — every slot holder
    /// transmits at least once per frame, so this tracks radio-range
    /// liveness closely.
    pub fn neighbors_heard(&self, now_us: u64) -> usize {
        let ttl = NEIGHBOR_TTL_FRAMES * self.config.frame_us();
        self.neighbors
            .values()
            .filter(|neighbor| now_us.saturating_sub(neighbor.last_heard_us) <= ttl)
            .count()
    }

    /// Record a received [`Hello`] (or the equivalent header piggybacked on a
    /// data packet). Returns true when the sender is *returning* — previously
    /// tracked, pruned for silence, now heard again — which callers use to
    /// trigger a recap (either side may hold messages from the time apart).
    pub fn on_hello(&mut self, now_us: u64, hello: &Hello, rssi_dbm: i16) -> bool {
        if hello.sender == self.node_id {
            return false;
        }
        self.prune(now_us);
        let returning = match self.lost.iter().position(|id| *id == hello.sender) {
            Some(i) => {
                self.lost.swap_remove(i);
                true
            }
            None => false,
        };
        let entry = Neighbor {
            slot: hello.slot,
            last_heard_us: now_us,
            rssi_dbm,
            neighbors: hello.neighbors.clone(),
        };
        // a full table drops the hello: the mesh is larger than we can track
        let _ = self.neighbors.insert(hello.sender, entry);
        returning
    }

    /// The current slot claim, picking (or re-picking after a lost conflict) a
    /// free data slot when needed. Call once per frame. Returns `None` when
    /// every data slot in the 2-hop neighborhood is taken.
    pub fn pick_slot(&mut self, now_us: u64) -> Option<u16> {
        self.prune(now_us);
        if let Some(slot) = self.my_slot {
            if self.yields_conflict(slot) {
                self.my_slot = None;
            } else {
                return Some(slot);
            }
        }
        let occupied = self.occupied();
        let first = self.config.first_data_slot();
        let count = self.config.slots_per_frame() - first;
        let seed = fnv1a(self.node_id.0) % u32::from(count);
        for i in 0..u32::from(count) {
            let candidate = first + ((seed + i) % u32::from(count)) as u16;
            if !occupied.contains(candidate) {
                self.my_slot = Some(candidate);
                return self.my_slot;
            }
        }
        None
    }

    /// The announcement this node should send.
    pub fn hello(&self) -> Hello {
        let mut neighbors = Vec::new();
        for (id, neighbor) in &self.neighbors {
            if let Some(slot) = neighbor.slot {
                // capacities match the table's, so push cannot fail
                let _ = neighbors.push((*id, slot));
            }
        }
        Hello {
            sender: self.node_id,
            slot: self.my_slot,
            neighbors,
        }
    }

    fn prune(&mut self, now_us: u64) {
        let ttl = NEIGHBOR_TTL_FRAMES * self.config.frame_us();
        let mut stale: Vec<NodeId, MAX_NEIGHBORS> = Vec::new();
        for (id, neighbor) in &self.neighbors {
            if now_us.saturating_sub(neighbor.last_heard_us) > ttl {
                let _ = stale.push(*id);
            }
        }
        for id in stale {
            self.neighbors.remove(&id);
            if !self.lost.contains(&id) {
                if self.lost.is_full() {
                    self.lost.remove(0);
                }
                let _ = self.lost.push(id);
            }
        }
    }

    /// Whether a node that outranks us (lower id) claims `slot` anywhere in
    /// the 2-hop neighborhood.
    fn yields_conflict(&self, slot: u16) -> bool {
        for (id, neighbor) in &self.neighbors {
            if neighbor.slot == Some(slot) && *id < self.node_id {
                return true;
            }
            for (two_hop_id, two_hop_slot) in &neighbor.neighbors {
                if *two_hop_slot == slot && *two_hop_id < self.node_id {
                    return true;
                }
            }
        }
        false
    }

    fn occupied(&self) -> SlotSet {
        let mut set = SlotSet::new();
        for neighbor in self.neighbors.values() {
            if let Some(slot) = neighbor.slot {
                set.insert(slot);
            }
            for (id, slot) in &neighbor.neighbors {
                if *id != self.node_id {
                    set.insert(*slot);
                }
            }
        }
        set
    }
}

/// Bitmap over the frame's slot indices (bounded by
/// [`super::MAX_SLOTS_PER_FRAME`]). Out-of-range slots from the wire are
/// ignored rather than trusted.
struct SlotSet {
    bits: [u64; 4],
}

impl SlotSet {
    fn new() -> Self {
        Self { bits: [0; 4] }
    }

    fn insert(&mut self, slot: u16) {
        let i = usize::from(slot);
        if i < 256 {
            self.bits[i / 64] |= 1 << (i % 64);
        }
    }

    fn contains(&self, slot: u16) -> bool {
        let i = usize::from(slot);
        i < 256 && self.bits[i / 64] & (1 << (i % 64)) != 0
    }
}

fn fnv1a(value: u32) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in value.to_le_bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tdma::{SlotKind, test_config};

    fn hello(sender: u32, slot: Option<u16>, neighbors: &[(u32, u16)]) -> Hello {
        let mut list = Vec::new();
        for (id, slot) in neighbors {
            list.push((NodeId(*id), *slot)).unwrap();
        }
        Hello {
            sender: NodeId(sender),
            slot,
            neighbors: list,
        }
    }

    #[test]
    fn fresh_node_picks_seeded_data_slot() {
        let config = test_config();
        let mut a = Coloring::new(config, NodeId(1));
        let mut b = Coloring::new(config, NodeId(2));
        let slot_a = a.pick_slot(0).unwrap();
        let slot_b = b.pick_slot(0).unwrap();
        assert_eq!(config.slot_kind(slot_a), SlotKind::Data);
        assert_eq!(config.slot_kind(slot_b), SlotKind::Data);
        assert_ne!(slot_a, slot_b);
        assert_eq!(a.pick_slot(0), Some(slot_a));
    }

    #[test]
    fn avoids_one_and_two_hop_claims() {
        let mut coloring = Coloring::new(test_config(), NodeId(9));
        let mine = coloring.pick_slot(0).unwrap();
        coloring.on_hello(0, &hello(100, Some(mine), &[(101, mine + 1)]), -60);
        // higher-id claims don't force a yield, but fresh picks avoid them
        assert_eq!(coloring.pick_slot(0), Some(mine));

        let mut fresh = Coloring::new(test_config(), NodeId(9));
        fresh.on_hello(0, &hello(100, Some(mine), &[(101, mine + 1)]), -60);
        let picked = fresh.pick_slot(0).unwrap();
        assert_ne!(picked, mine);
        assert_ne!(picked, mine + 1);
    }

    #[test]
    fn lower_id_wins_conflicts() {
        let mut coloring = Coloring::new(test_config(), NodeId(9));
        let mine = coloring.pick_slot(0).unwrap();
        coloring.on_hello(0, &hello(3, Some(mine), &[]), -60);
        let repicked = coloring.pick_slot(0).unwrap();
        assert_ne!(repicked, mine);

        // and via a 2-hop report
        coloring.on_hello(0, &hello(3, Some(mine), &[(4, repicked)]), -60);
        let repicked_again = coloring.pick_slot(0).unwrap();
        assert_ne!(repicked_again, repicked);
    }

    #[test]
    fn stale_neighbors_are_forgotten() {
        let config = test_config();
        let mut coloring = Coloring::new(config, NodeId(9));
        let mine = coloring.pick_slot(0).unwrap();
        coloring.on_hello(0, &hello(3, Some(mine), &[]), -60);
        assert_ne!(coloring.pick_slot(0), Some(mine));

        let after_ttl = 5 * config.frame_us();
        coloring.on_hello(after_ttl, &hello(50, Some(0), &[]), -60);
        assert!(!coloring.yields_conflict(mine));
    }

    #[test]
    fn hello_reports_table_and_own_slot() {
        let mut coloring = Coloring::new(test_config(), NodeId(9));
        let mine = coloring.pick_slot(0).unwrap();
        coloring.on_hello(0, &hello(3, Some(20), &[]), -60);
        coloring.on_hello(0, &hello(4, None, &[]), -60);

        let announced = coloring.hello();
        assert_eq!(announced.sender, NodeId(9));
        assert_eq!(announced.slot, Some(mine));
        assert_eq!(announced.neighbors.len(), 1);
        assert_eq!(announced.neighbors[0], (NodeId(3), 20));
    }

    #[test]
    fn saturated_neighborhood_returns_none() {
        let config = Config::new(160_000, 25, 15_000, 2, 3).unwrap();
        let mut coloring = Coloring::new(config, NodeId(9));
        let mut claims: [heapless::Vec<(u32, u16), 16>; 2] = [Vec::new(), Vec::new()];
        for slot in 5..25u16 {
            claims[usize::from(slot) % 2]
                .push((u32::from(slot) + 100, slot))
                .unwrap();
        }
        coloring.on_hello(0, &hello(1, None, &claims[0]), -60);
        coloring.on_hello(0, &hello(2, None, &claims[1]), -60);
        assert_eq!(coloring.pick_slot(0), None);
    }
}
