use std::fmt;

use sha2::{Digest, Sha256};

/// An 8-byte mesh identity: the first eight bytes of SHA-256 over the
/// peer's Noise static public key.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PeerId(pub [u8; 8]);

impl PeerId {
    pub const BROADCAST: PeerId = PeerId([0xFF; 8]);

    pub fn from_noise_key(noise_public: &[u8; 32]) -> PeerId {
        let digest = Sha256::digest(noise_public);
        let mut id = [0u8; 8];
        id.copy_from_slice(&digest[..8]);
        PeerId(id)
    }

    pub fn from_slice(bytes: &[u8]) -> Option<PeerId> {
        let arr: [u8; 8] = bytes.try_into().ok()?;
        Some(PeerId(arr))
    }

    pub fn from_hex(hex: &str) -> Option<PeerId> {
        if hex.len() != 16 {
            return None;
        }
        let mut id = [0u8; 8];
        for (i, byte) in id.iter_mut().enumerate() {
            *byte = u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()?;
        }
        Some(PeerId(id))
    }

    pub fn is_broadcast(&self) -> bool {
        *self == Self::BROADCAST
    }

    /// Lowercase hex, the form both apps use as the peer's string ID.
    pub fn hex(&self) -> String {
        hex(&self.0)
    }
}

impl fmt::Display for PeerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.hex())
    }
}

impl fmt::Debug for PeerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PeerId({})", self.hex())
    }
}

pub fn hex(bytes: &[u8]) -> String {
    use fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Full SHA-256 of the Noise static key in hex: what the apps show as the
/// peer's fingerprint.
pub fn fingerprint(noise_public: &[u8; 32]) -> String {
    hex(&Sha256::digest(noise_public))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trip() {
        let id = PeerId([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x00, 0x11]);
        assert_eq!(id.hex(), "aabbccddeeff0011");
        assert_eq!(PeerId::from_hex("aabbccddeeff0011"), Some(id));
        assert_eq!(PeerId::from_hex("AABBCCDDEEFF0011"), Some(id));
        assert_eq!(PeerId::from_hex("aabb"), None);
        assert_eq!(PeerId::from_hex("zzbbccddeeff0011"), None);
    }

    #[test]
    fn derived_from_sha256_prefix() {
        let key = [7u8; 32];
        let digest = Sha256::digest(key);
        assert_eq!(PeerId::from_noise_key(&key).0, digest[..8]);
        assert_eq!(fingerprint(&key), hex(&digest));
    }
}
