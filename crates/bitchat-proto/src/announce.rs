//! ANNOUNCE payload: a TLV list (`IdentityAnnouncement.kt`, iOS `Packets.swift`).
//!
//! `type u8 | len u8 | value`. Nickname (0x01), Noise key (0x02) and Ed25519
//! key (0x03) are required; unknown TLVs are kept verbatim.

use crate::peer_id::PeerId;

const TLV_NICKNAME: u8 = 0x01;
const TLV_NOISE_KEY: u8 = 0x02;
const TLV_SIGNING_KEY: u8 = 0x03;
const TLV_NEIGHBORS: u8 = 0x04;
const TLV_CAPABILITIES: u8 = 0x05;

/// At most this many direct neighbors ride in the gossip TLV.
pub const MAX_NEIGHBORS: usize = 10;

/// Feature bits, a minimal little-endian bitfield on the wire.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Capabilities(pub u64);

impl Capabilities {
    pub const PREKEYS: u64 = 1 << 0;
    pub const GROUPS: u64 = 1 << 3;
    pub const VOUCH: u64 = 1 << 5;
    pub const PRIVATE_MEDIA: u64 = 1 << 8;

    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut rest = self.0;
        loop {
            bytes.push(rest as u8);
            rest >>= 8;
            if rest == 0 {
                break;
            }
        }
        bytes
    }

    pub fn decode(data: &[u8]) -> Capabilities {
        let mut raw = 0u64;
        for (i, &b) in data.iter().take(8).enumerate() {
            raw |= (b as u64) << (8 * i);
        }
        Capabilities(raw)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Announcement {
    pub nickname: String,
    pub noise_public_key: Vec<u8>,
    pub signing_public_key: Vec<u8>,
    pub neighbors: Vec<PeerId>,
    pub capabilities: Option<Capabilities>,
    pub unknown: Vec<(u8, Vec<u8>)>,
}

impl Announcement {
    pub fn encode(&self) -> Option<Vec<u8>> {
        let nick = self.nickname.as_bytes();
        if nick.len() > 255
            || self.noise_public_key.len() > 255
            || self.signing_public_key.len() > 255
            || self.unknown.iter().any(|(_, v)| v.len() > 255)
        {
            return None;
        }
        let mut out = Vec::new();
        push_tlv(&mut out, TLV_NICKNAME, nick);
        push_tlv(&mut out, TLV_NOISE_KEY, &self.noise_public_key);
        push_tlv(&mut out, TLV_SIGNING_KEY, &self.signing_public_key);
        if let Some(caps) = self.capabilities {
            push_tlv(&mut out, TLV_CAPABILITIES, &caps.encode());
        }
        for (t, v) in &self.unknown {
            push_tlv(&mut out, *t, v);
        }
        // Android appends the neighbor gossip last.
        if !self.neighbors.is_empty() {
            let ids: Vec<u8> = self
                .neighbors
                .iter()
                .take(MAX_NEIGHBORS)
                .flat_map(|p| p.0)
                .collect();
            push_tlv(&mut out, TLV_NEIGHBORS, &ids);
        }
        Some(out)
    }

    pub fn decode(data: &[u8]) -> Option<Announcement> {
        let mut nickname = None;
        let mut noise = None;
        let mut signing = None;
        let mut neighbors = Vec::new();
        let mut capabilities = None;
        let mut unknown = Vec::new();

        let mut i = 0;
        while i + 2 <= data.len() {
            let t = data[i];
            let len = data[i + 1] as usize;
            i += 2;
            let value = data.get(i..i + len)?;
            i += len;
            match t {
                TLV_NICKNAME => nickname = Some(String::from_utf8_lossy(value).into_owned()),
                TLV_NOISE_KEY => noise = Some(value.to_vec()),
                TLV_SIGNING_KEY => signing = Some(value.to_vec()),
                TLV_CAPABILITIES => capabilities = Some(Capabilities::decode(value)),
                TLV_NEIGHBORS => {
                    neighbors = value.as_chunks::<8>().0.iter().map(|c| PeerId(*c)).collect();
                }
                _ => unknown.push((t, value.to_vec())),
            }
        }

        Some(Announcement {
            nickname: nickname?,
            noise_public_key: noise?,
            signing_public_key: signing?,
            neighbors,
            capabilities,
            unknown,
        })
    }

    /// The Noise key as a fixed array, when it is the right size.
    pub fn noise_key(&self) -> Option<[u8; 32]> {
        self.noise_public_key.as_slice().try_into().ok()
    }

    pub fn signing_key(&self) -> Option<[u8; 32]> {
        self.signing_public_key.as_slice().try_into().ok()
    }
}

fn push_tlv(out: &mut Vec<u8>, t: u8, v: &[u8]) {
    out.push(t);
    out.push(v.len() as u8);
    out.extend_from_slice(v);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Announcement {
        Announcement {
            nickname: "raven".into(),
            noise_public_key: vec![1; 32],
            signing_public_key: vec![2; 32],
            neighbors: vec![PeerId([3; 8]), PeerId([4; 8])],
            capabilities: Some(Capabilities(Capabilities::PRIVATE_MEDIA)),
            unknown: vec![(0x06, b"u4pru".to_vec())],
        }
    }

    #[test]
    fn layout() {
        let bytes = sample().encode().unwrap();
        assert_eq!(&bytes[..7], &[0x01, 5, b'r', b'a', b'v', b'e', b'n']);
        assert_eq!(&bytes[7..9], &[0x02, 32]);
        assert_eq!(&bytes[41..43], &[0x03, 32]);
        // Capabilities: bit 8 -> two little-endian bytes.
        assert_eq!(&bytes[75..79], &[0x05, 2, 0x00, 0x01]);
    }

    #[test]
    fn round_trip() {
        let a = sample();
        assert_eq!(Announcement::decode(&a.encode().unwrap()).unwrap(), a);
    }

    #[test]
    fn requires_all_three_fields() {
        let mut bytes = Vec::new();
        push_tlv(&mut bytes, TLV_NICKNAME, b"x");
        push_tlv(&mut bytes, TLV_NOISE_KEY, &[0; 32]);
        assert!(Announcement::decode(&bytes).is_none());
    }

    #[test]
    fn rejects_truncated_tlv() {
        let bytes = sample().encode().unwrap();
        assert!(Announcement::decode(&bytes[..bytes.len() - 1]).is_none());
    }

    #[test]
    fn capabilities_minimal_encoding() {
        assert_eq!(Capabilities(0).encode(), vec![0]);
        assert_eq!(Capabilities(0x29).encode(), vec![0x29]);
        assert_eq!(Capabilities::decode(&[0x00, 0x01]).0, 0x100);
    }
}
