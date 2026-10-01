//! Fragmentation and reassembly (`FragmentPayload.kt`, `FragmentManager.kt`).
//!
//! A FRAGMENT payload is `fragmentID[8] | index u16 | total u16 | originalType u8 | data`.
//! The data chunks are slices of the complete encoded original frame
//! (signature included, unpadded). Fragments copy the original's sender,
//! recipient, timestamp, TTL and route, and are never signed.

use std::collections::HashMap;

use rand::RngCore;

use crate::packet::{MessageType, Packet};

pub const HEADER_LEN: usize = 13;
pub const MAX_FRAGMENT_DATA: usize = 469;
pub const MAX_FRAGMENTS_PER_ID: usize = 256;
pub const MAX_SET_BYTES: usize = 1024 * 1024;
pub const MAX_ACTIVE_SETS: usize = 64;
pub const MAX_GLOBAL_BYTES: usize = 4 * 1024 * 1024;
pub const TIMEOUT_MS: u64 = 30_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FragmentPayload {
    pub id: [u8; 8],
    pub index: u16,
    pub total: u16,
    pub original_type: u8,
    pub data: Vec<u8>,
}

impl FragmentPayload {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN + self.data.len());
        out.extend_from_slice(&self.id);
        out.extend_from_slice(&self.index.to_be_bytes());
        out.extend_from_slice(&self.total.to_be_bytes());
        out.push(self.original_type);
        out.extend_from_slice(&self.data);
        out
    }

    pub fn decode(payload: &[u8]) -> Option<FragmentPayload> {
        if payload.len() < HEADER_LEN {
            return None;
        }
        let f = FragmentPayload {
            id: payload[..8].try_into().ok()?,
            index: u16::from_be_bytes([payload[8], payload[9]]),
            total: u16::from_be_bytes([payload[10], payload[11]]),
            original_type: payload[12],
            data: payload[HEADER_LEN..].to_vec(),
        };
        (f.total > 0 && f.index < f.total && !f.data.is_empty()).then_some(f)
    }
}

/// Split a packet whose unpadded frame exceeds `max_frame` bytes. Returns
/// the packet itself (as a one-element list) when it already fits.
///
/// `max_frame` is 512 for Android-sized links, or a smaller per-link ATT
/// limit. Chunks are sized so every fragment frame fits `max_frame`
/// including 16 bytes of padding slack, capped at 469 as both apps do.
pub fn split(packet: &Packet, max_frame: usize) -> Option<Vec<Packet>> {
    // Android pads and then unpads here, which is the same bytes except when
    // the frame was too big to pad and its last signature byte happens to
    // look like padding (about 1 in 256): then it strips real data. Encoding
    // unpadded avoids that.
    let full = packet.encode(false).ok()?;
    if full.len() <= max_frame {
        return Some(vec![packet.clone()]);
    }

    let routed = !packet.route.is_empty();
    let header = if routed { 16 } else { 14 };
    let recipient = if packet.recipient.is_some() { 8 } else { 0 };
    let route = if routed { 1 + packet.route.len() * 8 } else { 0 };
    let overhead = header + 8 + recipient + route + HEADER_LEN + 16;
    let chunk = max_frame.checked_sub(overhead)?.min(MAX_FRAGMENT_DATA);
    if chunk == 0 {
        return None;
    }
    let total = full.len().div_ceil(chunk);
    if total > u16::MAX as usize {
        return None;
    }

    let mut id = [0u8; 8];
    rand::thread_rng().fill_bytes(&mut id);
    let fragments = full
        .chunks(chunk)
        .enumerate()
        .map(|(index, data)| Packet {
            version: if routed { 2 } else { 1 },
            ptype: MessageType::Fragment as u8,
            ttl: packet.ttl,
            timestamp: packet.timestamp,
            sender: packet.sender,
            recipient: packet.recipient,
            route: packet.route.clone(),
            payload: FragmentPayload {
                id,
                index: index as u16,
                total: total as u16,
                original_type: packet.ptype,
                data: data.to_vec(),
            }
            .encode(),
            signature: None,
            // iOS checks every fragment's age; a sync reply's fragments
            // need the flag as much as the reply does.
            rsr: packet.rsr,
            wire: None,
        })
        .collect();
    Some(fragments)
}

struct Assembly {
    original_type: u8,
    total: u16,
    parts: HashMap<u16, Vec<u8>>,
    bytes: usize,
    started_ms: u64,
}

/// Collects fragments into complete packets, bounded like Android's so a
/// peer can't make us buffer without limit.
#[derive(Default)]
pub struct Reassembler {
    sets: HashMap<[u8; 8], Assembly>,
    global_bytes: usize,
}

impl Reassembler {
    pub fn new() -> Reassembler {
        Reassembler::default()
    }

    /// Feed one FRAGMENT packet. Returns the reassembled packet, with TTL 0
    /// so it is not relayed again (the fragments themselves are relayed).
    pub fn push(&mut self, fragment: &Packet, now_ms: u64) -> Option<Packet> {
        let f = FragmentPayload::decode(&fragment.payload)?;
        if f.total as usize > MAX_FRAGMENTS_PER_ID {
            return None;
        }

        if let Some(set) = self.sets.get(&f.id) {
            if set.total != f.total || set.original_type != f.original_type {
                self.remove(&f.id);
                return None;
            }
        } else {
            if self.sets.len() >= MAX_ACTIVE_SETS {
                return None;
            }
            self.sets.insert(
                f.id,
                Assembly {
                    original_type: f.original_type,
                    total: f.total,
                    parts: HashMap::new(),
                    bytes: 0,
                    started_ms: now_ms,
                },
            );
        }

        let set = self.sets.get_mut(&f.id)?;
        let old = set.parts.get(&f.index).map_or(0, Vec::len);
        let new_bytes = set.bytes - old + f.data.len();
        let new_global = self.global_bytes - old + f.data.len();
        if new_bytes > MAX_SET_BYTES || new_global > MAX_GLOBAL_BYTES {
            self.remove(&f.id);
            return None;
        }
        set.bytes = new_bytes;
        self.global_bytes = new_global;
        set.parts.insert(f.index, f.data);

        if set.parts.len() < set.total as usize {
            return None;
        }
        let mut whole = Vec::with_capacity(set.bytes);
        for i in 0..set.total {
            whole.extend_from_slice(set.parts.get(&i)?);
        }
        self.remove(&f.id);
        let mut packet = Packet::decode(&whole)?;
        packet.ttl = 0;
        Some(packet)
    }

    /// Drop incomplete sets older than the timeout.
    pub fn expire(&mut self, now_ms: u64) {
        let stale: Vec<[u8; 8]> = self
            .sets
            .iter()
            .filter(|(_, s)| now_ms.saturating_sub(s.started_ms) > TIMEOUT_MS)
            .map(|(id, _)| *id)
            .collect();
        for id in stale {
            self.remove(&id);
        }
    }

    pub fn active_sets(&self) -> usize {
        self.sets.len()
    }

    fn remove(&mut self, id: &[u8; 8]) {
        if let Some(set) = self.sets.remove(id) {
            self.global_bytes = self.global_bytes.saturating_sub(set.bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{self, Identity};

    fn long_message(id: &Identity, len: usize) -> Packet {
        // Random text so compression can't shrink it under the limit.
        use rand::{Rng, SeedableRng};
        let mut rng = rand::rngs::StdRng::seed_from_u64(len as u64);
        let text: String = (0..len).map(|_| char::from(rng.gen_range(b'!'..=b'~'))).collect();
        id.message_packet(&text)
    }

    #[test]
    fn small_packets_are_not_split() {
        let id = Identity::generate();
        let p = id.message_packet("short");
        assert_eq!(split(&p, 512).unwrap(), vec![p]);
    }

    #[test]
    fn split_and_reassemble_signed_message() {
        let id = Identity::generate();
        let original = long_message(&id, 1500);
        let frags = split(&original, 512).unwrap();
        assert!(frags.len() > 1);
        for f in &frags {
            assert!(f.encode_for_ble().unwrap().len() <= 512);
            assert!(f.signature.is_none());
            assert_eq!(f.ptype, MessageType::Fragment as u8);
        }

        let mut r = Reassembler::new();
        let mut out = None;
        // Out-of-order delivery is fine.
        for f in frags.iter().rev() {
            let wire = Packet::decode(&f.encode_for_ble().unwrap()).unwrap();
            out = r.push(&wire, 0).or(out);
        }
        let whole = out.unwrap();
        assert_eq!(whole.payload, original.payload);
        assert_eq!(whole.ttl, 0);
        assert!(identity::verify(&whole, &id.signing_public()));
        assert_eq!(r.active_sets(), 0);
    }

    #[test]
    fn frame_ending_in_pad_like_byte_survives() {
        // Too big to pad, and the signature's last byte reads as one byte of
        // PKCS#7 padding. Pad-then-unpad (Android's way) would cut it off.
        let id = Identity::generate();
        let mut p = long_message(&id, 2100);
        let mut sig = [0x5a; 64];
        sig[63] = 0x01;
        p.signature = Some(sig);
        let frags = split(&p, 512).unwrap();
        let mut r = Reassembler::new();
        let whole = frags.iter().find_map(|f| r.push(f, 0)).unwrap();
        assert_eq!(whole.signature, Some(sig));
        assert_eq!(whole.payload, p.payload);
    }

    #[test]
    fn fragments_keep_rsr() {
        let id = Identity::generate();
        let mut p = long_message(&id, 900);
        p.rsr = true;
        let frags = split(&p, 182).unwrap();
        assert!(frags.len() > 1 && frags.iter().all(|f| f.rsr));
    }

    #[test]
    fn smaller_link_limit_gives_smaller_frames() {
        let id = Identity::generate();
        let original = long_message(&id, 600);
        let frags = split(&original, 185).unwrap();
        for f in &frags {
            assert!(f.encode_for_ble().unwrap().len() <= 185);
        }
    }

    #[test]
    fn rejects_inconsistent_metadata() {
        let mut r = Reassembler::new();
        let mk = |total: u16, index: u16| {
            let mut p = Packet::new(MessageType::Fragment, crate::PeerId([1; 8]), Vec::new());
            p.payload = FragmentPayload { id: [5; 8], index, total, original_type: 2, data: vec![1] }.encode();
            p
        };
        assert!(r.push(&mk(3, 0), 0).is_none());
        assert_eq!(r.active_sets(), 1);
        assert!(r.push(&mk(4, 1), 0).is_none());
        assert_eq!(r.active_sets(), 0);
    }

    #[test]
    fn rejects_too_many_fragments_and_expires() {
        let mut r = Reassembler::new();
        let mut p = Packet::new(MessageType::Fragment, crate::PeerId([1; 8]), Vec::new());
        p.payload = FragmentPayload { id: [5; 8], index: 0, total: 257, original_type: 2, data: vec![1] }.encode();
        assert!(r.push(&p, 0).is_none());
        assert_eq!(r.active_sets(), 0);

        p.payload = FragmentPayload { id: [6; 8], index: 0, total: 2, original_type: 2, data: vec![1] }.encode();
        r.push(&p, 0);
        r.expire(TIMEOUT_MS);
        assert_eq!(r.active_sets(), 1);
        r.expire(TIMEOUT_MS + 1);
        assert_eq!(r.active_sets(), 0);
    }
}
