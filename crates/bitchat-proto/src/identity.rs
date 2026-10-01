//! Our keys, and packet signing and verification.
//!
//! Each node holds a Curve25519 static key (Noise; the peer ID derives from
//! it) and a separate Ed25519 signing key. Public packets are signed over
//! [`Packet::signing_preimage`].

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand::rngs::OsRng;
use x25519_dalek::{PublicKey as XPublic, StaticSecret};

use crate::announce::{Announcement, Capabilities};
use crate::packet::{MessageType, Packet};
use crate::peer_id::{self, PeerId};

pub struct Identity {
    noise_secret: StaticSecret,
    signing: SigningKey,
}

impl Identity {
    pub fn generate() -> Identity {
        Identity {
            noise_secret: StaticSecret::random_from_rng(OsRng),
            signing: SigningKey::generate(&mut OsRng),
        }
    }

    pub fn from_secrets(noise_secret: [u8; 32], signing_secret: [u8; 32]) -> Identity {
        Identity {
            noise_secret: StaticSecret::from(noise_secret),
            signing: SigningKey::from_bytes(&signing_secret),
        }
    }

    pub fn noise_secret_bytes(&self) -> [u8; 32] {
        self.noise_secret.to_bytes()
    }

    pub fn signing_secret_bytes(&self) -> [u8; 32] {
        self.signing.to_bytes()
    }

    pub fn noise_public(&self) -> [u8; 32] {
        XPublic::from(&self.noise_secret).to_bytes()
    }

    pub fn signing_public(&self) -> [u8; 32] {
        self.signing.verifying_key().to_bytes()
    }

    pub fn peer_id(&self) -> PeerId {
        PeerId::from_noise_key(&self.noise_public())
    }

    pub fn fingerprint(&self) -> String {
        peer_id::fingerprint(&self.noise_public())
    }

    /// Attach a signature over the packet's canonical preimage.
    pub fn sign(&self, packet: &mut Packet) {
        if let Ok(preimage) = packet.signing_preimage() {
            packet.signature = Some(self.signing.sign(&preimage).to_bytes());
        }
    }

    /// A signed announce for us, as sent on every new link and periodically.
    pub fn announce_packet(&self, nickname: &str, neighbors: Vec<PeerId>) -> Packet {
        let payload = Announcement {
            nickname: nickname.to_owned(),
            noise_public_key: self.noise_public().to_vec(),
            signing_public_key: self.signing_public().to_vec(),
            neighbors,
            capabilities: Some(Capabilities(0)),
            unknown: Vec::new(),
        }
        .encode()
        .unwrap_or_default();
        let mut packet = Packet::new(MessageType::Announce, self.peer_id(), payload);
        self.sign(&mut packet);
        packet
    }

    /// A signed public chat message. Android addresses these to the
    /// broadcast ID; iOS omits the recipient. Both accept either.
    pub fn message_packet(&self, text: &str) -> Packet {
        let mut packet = Packet::new(MessageType::Message, self.peer_id(), text.as_bytes().to_vec());
        packet.recipient = Some(PeerId::BROADCAST);
        self.sign(&mut packet);
        packet
    }

    pub fn leave_packet(&self) -> Packet {
        let mut packet = Packet::new(MessageType::Leave, self.peer_id(), Vec::new());
        self.sign(&mut packet);
        packet
    }
}

/// Check a packet's signature against a known Ed25519 key.
pub fn verify(packet: &Packet, signing_public: &[u8; 32]) -> bool {
    let Some(sig) = packet.signature else {
        return false;
    };
    let Ok(key) = VerifyingKey::from_bytes(signing_public) else {
        return false;
    };
    let Ok(preimage) = packet.signing_preimage() else {
        return false;
    };
    key.verify(&preimage, &Signature::from_bytes(&sig)).is_ok()
}

/// Validate a self-signed ANNOUNCE (`AnnouncementIdentityValidator.kt`):
/// decodes, the sender ID must match the announced Noise key, and the
/// signature must verify under the announced signing key.
pub fn verify_announce(packet: &Packet) -> Option<Announcement> {
    if packet.ptype != MessageType::Announce as u8 {
        return None;
    }
    let ann = Announcement::decode(&packet.payload)?;
    let noise = ann.noise_key()?;
    let signing = ann.signing_key()?;
    if PeerId::from_noise_key(&noise) != packet.sender {
        return None;
    }
    verify(packet, &signing).then_some(ann)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announce_verifies() {
        let id = Identity::generate();
        let packet = id.announce_packet("raven", vec![PeerId([9; 8])]);
        let wire = packet.encode_for_ble().unwrap();
        let back = Packet::decode(&wire).unwrap();
        let ann = verify_announce(&back).unwrap();
        assert_eq!(ann.nickname, "raven");
        assert_eq!(ann.neighbors, vec![PeerId([9; 8])]);
        assert_eq!(back.sender, id.peer_id());
        assert!(back.recipient.is_none());
    }

    #[test]
    fn relayed_ttl_still_verifies() {
        let id = Identity::generate();
        let mut packet = Packet::decode(&id.message_packet("hello").encode_for_ble().unwrap()).unwrap();
        packet.ttl = 3;
        assert!(verify(&packet, &id.signing_public()));
    }

    #[test]
    fn tampering_breaks_signature() {
        let id = Identity::generate();
        let mut packet = id.message_packet("hello");
        packet.payload = b"hellp".to_vec();
        assert!(!verify(&packet, &id.signing_public()));
    }

    #[test]
    fn long_compressed_message_verifies_after_round_trip() {
        let id = Identity::generate();
        let text = "the quick brown fox jumps over the lazy dog ".repeat(8);
        let packet = id.message_packet(&text);
        let back = Packet::decode(&packet.encode_for_ble().unwrap()).unwrap();
        assert!(back.wire.as_ref().unwrap().compressed);
        assert!(verify(&back, &id.signing_public()));
    }

    #[test]
    fn announce_with_foreign_sender_rejected() {
        let id = Identity::generate();
        let mut packet = id.announce_packet("x", vec![]);
        packet.sender = PeerId([1; 8]);
        id.sign(&mut packet);
        assert!(verify_announce(&packet).is_none());
    }

    #[test]
    fn secrets_round_trip() {
        let id = Identity::generate();
        let again = Identity::from_secrets(id.noise_secret_bytes(), id.signing_secret_bytes());
        assert_eq!(again.peer_id(), id.peer_id());
        assert_eq!(again.signing_public(), id.signing_public());
    }
}
