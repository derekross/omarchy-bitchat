//! Relay decisions: Android's `PacketRelayManager.kt` probability table
//! plus iOS's jitter (`RelayController.swift`).

use std::time::Duration;

use rand::Rng;

use crate::packet::{MessageType, Packet};
use crate::peer_id::PeerId;

#[derive(Debug, PartialEq, Eq)]
pub enum RelayDecision {
    /// Don't forward.
    Drop,
    /// Forward this packet (TTL already decremented) to the named next hop
    /// if it is directly connected, otherwise flood.
    NextHop(PeerId, Packet),
    /// Flood to every link except the one it came in on, after a delay.
    Flood(Packet, Duration),
}

/// Decide what to do with a validated packet not addressed to us.
/// `network_size` is the number of known peers.
pub fn decide(packet: &Packet, me: PeerId, network_size: usize, rng: &mut impl Rng) -> RelayDecision {
    if packet.sender == me || packet.ttl <= 1 {
        return RelayDecision::Drop;
    }
    if packet.ptype == MessageType::RequestSync as u8 {
        return RelayDecision::Drop;
    }
    if let Some(r) = packet.recipient
        && r == me {
            return RelayDecision::Drop;
        }

    let mut out = packet.clone();
    out.ttl = out.ttl.min(crate::MAX_TTL) - 1;
    // RSR marks a reply to one neighbor's sync request, not something to flood.
    out.rsr = false;

    if !out.route.is_empty() {
        let mut unique = out.route.clone();
        unique.sort();
        unique.dedup();
        if unique.len() < out.route.len() {
            return RelayDecision::Drop; // loop
        }
        if let Some(pos) = out.route.iter().position(|&h| h == me) {
            let next = out.route.get(pos + 1).copied().or(out.recipient);
            if let Some(next) = next
                && !next.is_broadcast() {
                    return RelayDecision::NextHop(next, out);
                }
        }
    }

    let relay = out.ttl >= 4 || network_size <= 10 || {
        let p = if network_size <= 30 {
            0.85
        } else if network_size <= 50 {
            0.7
        } else if network_size <= 100 {
            0.55
        } else {
            0.4
        };
        rng.gen_bool(p)
    };
    if !relay {
        return RelayDecision::Drop;
    }

    let jitter_ms = if out.ptype == MessageType::Fragment as u8 || out.ptype == MessageType::VoiceFrame as u8 {
        rng.gen_range(8..=25)
    } else if !out.is_broadcast() || out.ptype == MessageType::NoiseHandshake as u8 {
        rng.gen_range(10..=35)
    } else {
        rng.gen_range(10..=220)
    };
    RelayDecision::Flood(out, Duration::from_millis(jitter_ms))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    fn me() -> PeerId {
        PeerId([0xAA; 8])
    }

    fn pkt(ttl: u8) -> Packet {
        let mut p = Packet::new(MessageType::Message, PeerId([1; 8]), b"x".to_vec());
        p.ttl = ttl;
        p
    }

    #[test]
    fn floods_with_decremented_ttl() {
        let mut rng = StdRng::seed_from_u64(1);
        match decide(&pkt(7), me(), 3, &mut rng) {
            RelayDecision::Flood(p, d) => {
                assert_eq!(p.ttl, 6);
                assert!(d >= Duration::from_millis(10) && d <= Duration::from_millis(220));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn drops_expiring_own_and_sync() {
        let mut rng = StdRng::seed_from_u64(1);
        assert_eq!(decide(&pkt(1), me(), 3, &mut rng), RelayDecision::Drop);
        assert_eq!(decide(&pkt(0), me(), 3, &mut rng), RelayDecision::Drop);
        let mut own = pkt(7);
        own.sender = me();
        assert_eq!(decide(&own, me(), 3, &mut rng), RelayDecision::Drop);
        let mut sync = pkt(7);
        sync.ptype = MessageType::RequestSync as u8;
        assert_eq!(decide(&sync, me(), 3, &mut rng), RelayDecision::Drop);
        let mut to_me = pkt(7);
        to_me.recipient = Some(me());
        assert_eq!(decide(&to_me, me(), 3, &mut rng), RelayDecision::Drop);
    }

    #[test]
    fn follows_source_route() {
        let mut rng = StdRng::seed_from_u64(1);
        let next = PeerId([0xBB; 8]);
        let mut p = pkt(7);
        p.version = 2;
        p.recipient = Some(PeerId([0xCC; 8]));
        p.route = vec![me(), next];
        assert!(matches!(decide(&p, me(), 3, &mut rng), RelayDecision::NextHop(n, _) if n == next));
        // Last intermediate hop forwards to the recipient.
        p.route = vec![me()];
        assert!(matches!(decide(&p, me(), 3, &mut rng), RelayDecision::NextHop(n, _) if n == PeerId([0xCC; 8])));
        // Loops are dropped.
        p.route = vec![me(), next, me()];
        assert_eq!(decide(&p, me(), 3, &mut rng), RelayDecision::Drop);
    }

    #[test]
    fn large_networks_relay_low_ttl_probabilistically() {
        let mut rng = StdRng::seed_from_u64(7);
        let relayed = (0..1000)
            .filter(|_| matches!(decide(&pkt(3), me(), 200, &mut rng), RelayDecision::Flood(..)))
            .count();
        assert!((300..500).contains(&relayed), "{relayed}");
    }
}
