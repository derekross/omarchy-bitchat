//! Packet IDs and the seen-set (`PacketIdUtil.kt`, `SecurityManager.kt`).

use std::collections::{HashMap, VecDeque};

use sha2::{Digest, Sha256};

use crate::packet::Packet;

pub const SEEN_TTL_MS: u64 = 300_000;
pub const SEEN_CAPACITY: usize = 10_000;

/// `SHA256(type | senderID | timestamp_be8 | payload)[..16]`: stable across
/// relays (TTL, route and signature are excluded). Also the ID gossip sync
/// uses.
pub fn packet_id(packet: &Packet) -> [u8; 16] {
    let mut h = Sha256::new();
    h.update([packet.ptype]);
    h.update(packet.sender.0);
    h.update(packet.timestamp.to_be_bytes());
    h.update(&packet.payload);
    let digest = h.finalize();
    let mut id = [0u8; 16];
    id.copy_from_slice(&digest[..16]);
    id
}

/// Time- and size-bounded set of packet IDs already handled.
#[derive(Default)]
pub struct SeenSet {
    seen: HashMap<[u8; 16], u64>,
    order: VecDeque<([u8; 16], u64)>,
}

impl SeenSet {
    pub fn new() -> SeenSet {
        SeenSet::default()
    }

    pub fn contains(&self, id: &[u8; 16]) -> bool {
        self.seen.contains_key(id)
    }

    /// Record an ID. Only call this for packets that passed validation, so a
    /// forged packet can't poison the set for the genuine one.
    pub fn insert(&mut self, id: [u8; 16], now_ms: u64) {
        if self.seen.insert(id, now_ms).is_none() {
            self.order.push_back((id, now_ms));
        }
        self.prune(now_ms);
    }

    pub fn prune(&mut self, now_ms: u64) {
        while let Some(&(id, at)) = self.order.front() {
            let expired = now_ms.saturating_sub(at) > SEEN_TTL_MS;
            if !expired && self.order.len() <= SEEN_CAPACITY {
                break;
            }
            self.order.pop_front();
            if self.seen.get(&id) == Some(&at) {
                self.seen.remove(&id);
            }
        }
    }

    pub fn len(&self) -> usize {
        self.seen.len()
    }

    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::MessageType;
    use crate::peer_id::PeerId;

    #[test]
    fn id_ignores_ttl_and_signature() {
        let mut a = Packet::new(MessageType::Message, PeerId([1; 8]), b"x".to_vec());
        let mut b = a.clone();
        b.ttl = 2;
        b.signature = Some([0; 64]);
        assert_eq!(packet_id(&a), packet_id(&b));
        a.payload = b"y".to_vec();
        assert_ne!(packet_id(&a), packet_id(&b));
    }

    #[test]
    fn expires_and_caps() {
        let mut s = SeenSet::new();
        s.insert([1; 16], 0);
        assert!(s.contains(&[1; 16]));
        s.prune(SEEN_TTL_MS + 1);
        assert!(!s.contains(&[1; 16]));

        for i in 0..(SEEN_CAPACITY + 5) as u32 {
            let mut id = [0u8; 16];
            id[..4].copy_from_slice(&i.to_be_bytes());
            s.insert(id, 1);
        }
        assert_eq!(s.len(), SEEN_CAPACITY);
    }
}
