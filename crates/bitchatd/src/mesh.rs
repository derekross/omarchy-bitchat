//! The mesh engine: what to do with each frame, link change and tick.
//!
//! Pure state machine: no Bluetooth, no clocks, no tasks. The transport feeds
//! it frames and link events; it answers with [`Effect`]s (frames to send,
//! events for the UI). That keeps every protocol decision unit-testable.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Duration;

use bitchat_proto::dedup::{SeenSet, packet_id};
use bitchat_proto::fragment::Reassembler;
use bitchat_proto::identity::{self, Identity};
use bitchat_proto::peer_id::hex;
use bitchat_proto::relay::{self, RelayDecision};
use bitchat_proto::sync::{self as gsync, RequestSync};
use bitchat_proto::{MAX_TTL, MessageType, Packet, PeerId};
use rand::Rng;
use serde::Serialize;

/// Transport-assigned handle for one GATT link (either role).
pub type LinkId = u64;

pub const PEER_STALE_MS: u64 = 180_000;
/// A peer that lost its last direct link and has been silent this long is gone.
pub const DISCONNECT_GRACE_MS: u64 = 10_000;
pub const ANNOUNCE_INTERVAL_MS: u64 = 30_000;
pub const ANNOUNCE_JITTER_MS: u64 = 5_000;
/// Announce on a fresh link after this delay (Android waits 200 ms).
pub const LINK_ANNOUNCE_DELAY: Duration = Duration::from_millis(200);
pub const ANNOUNCE_MAX_SKEW_MS: u64 = 600_000;
/// Oldest public message we still show (iOS's broadcast freshness window).
pub const MESSAGE_MAX_AGE_MS: u64 = 6 * 3_600_000;
/// Newer than this we relay; older packets are shown but not re-flooded.
pub const RELAY_MAX_AGE_MS: u64 = 120_000;
pub const FUTURE_SKEW_MS: u64 = 600_000;
/// Messages from a peer whose announce hasn't arrived wait this long.
pub const PENDING_MS: u64 = 30_000;
pub const PENDING_MAX: usize = 64;
pub const LOG_MAX: usize = 500;
/// Ask neighbors what we're missing this often, and this soon after a new
/// neighbor turns up.
pub const SYNC_INTERVAL_MS: u64 = 30_000;
pub const SYNC_FIRST_DELAY: Duration = Duration::from_secs(1);
/// Broadcast messages kept for answering sync requests.
pub const SYNC_STORE_MAX: usize = 500;
/// Answer one peer at most this often.
pub const SYNC_REPLY_COOLDOWN_MS: u64 = 10_000;
pub const SYNC_REPLY_MAX: usize = 300;
pub const SYNC_REPLY_PACING_MS: u64 = 25;
pub const NICKNAME_MAX: usize = 15;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// Every link.
    All,
    /// Every link but the one the packet came in on.
    AllExcept(LinkId),
    /// One link.
    Link(LinkId),
}

#[derive(Debug)]
pub enum Effect {
    Send { packet: Packet, target: Target, delay: Duration },
    Event(Event),
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(tag = "event", content = "data", rename_all = "camelCase")]
pub enum Event {
    Message(ChatMessage),
    Peers(Vec<PeerView>),
    Identity(Me),
}

#[derive(Clone, Debug, Serialize, serde::Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    pub id: String,
    pub sender_id: String,
    pub nickname: String,
    pub text: String,
    pub timestamp: u64,
    pub mine: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PeerView {
    pub id: String,
    pub nickname: String,
    pub fingerprint: String,
    pub direct: bool,
    pub last_seen: u64,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Me {
    pub peer_id: String,
    pub nickname: String,
    pub fingerprint: String,
}

struct Peer {
    nickname: String,
    noise_key: [u8; 32],
    signing_key: [u8; 32],
    last_seen: u64,
    links: HashSet<LinkId>,
    /// When the last direct link went away, if it has.
    unlinked_at: Option<u64>,
}

pub struct Mesh {
    me: Identity,
    my_id: PeerId,
    nickname: String,
    peers: HashMap<PeerId, Peer>,
    link_peer: HashMap<LinkId, PeerId>,
    links: HashSet<LinkId>,
    seen: SeenSet,
    reassembler: Reassembler,
    pending: VecDeque<(Packet, LinkId, u64)>,
    log: VecDeque<ChatMessage>,
    logged_ids: HashSet<String>,
    next_announce: u64,
    /// What we can hand a neighbor that asks: broadcast messages and the
    /// latest announce per peer, as received (signatures intact).
    sync_msgs: VecDeque<Packet>,
    sync_ids: HashSet<[u8; 16]>,
    sync_announces: HashMap<PeerId, Packet>,
    sync_replied: HashMap<PeerId, u64>,
    next_sync: u64,
    cleared_before: u64,
}

impl Mesh {
    pub fn new(me: Identity, nickname: String, history: Vec<ChatMessage>) -> Mesh {
        let my_id = me.peer_id();
        let mut mesh = Mesh {
            me,
            my_id,
            nickname,
            peers: HashMap::new(),
            link_peer: HashMap::new(),
            links: HashSet::new(),
            seen: SeenSet::new(),
            reassembler: Reassembler::new(),
            pending: VecDeque::new(),
            log: VecDeque::new(),
            logged_ids: HashSet::new(),
            next_announce: 0,
            sync_msgs: VecDeque::new(),
            sync_ids: HashSet::new(),
            sync_announces: HashMap::new(),
            sync_replied: HashMap::new(),
            next_sync: 0,
            cleared_before: 0,
        };
        for m in history {
            mesh.push_log(m);
        }
        mesh
    }

    #[cfg(test)]
    pub fn peer_id(&self) -> PeerId {
        self.my_id
    }

    pub fn me(&self) -> Me {
        Me {
            peer_id: self.my_id.hex(),
            nickname: self.nickname.clone(),
            fingerprint: self.me.fingerprint(),
        }
    }

    pub fn nickname(&self) -> &str {
        &self.nickname
    }

    pub fn identity(&self) -> &Identity {
        &self.me
    }

    pub fn messages(&self) -> impl Iterator<Item = &ChatMessage> {
        self.log.iter()
    }

    pub fn link_count(&self) -> usize {
        self.links.len()
    }

    pub fn peers(&self) -> Vec<PeerView> {
        let mut out: Vec<PeerView> = self
            .peers
            .iter()
            .map(|(id, p)| PeerView {
                id: id.hex(),
                nickname: p.nickname.clone(),
                fingerprint: bitchat_proto::peer_id::fingerprint(&p.noise_key),
                direct: !p.links.is_empty(),
                last_seen: p.last_seen,
            })
            .collect();
        out.sort_by_key(|a| a.nickname.to_lowercase());
        out
    }

    /// Peer ID bound to a link by a direct (TTL 7) announce, if any.
    pub fn link_peer(&self, link: LinkId) -> Option<PeerId> {
        self.link_peer.get(&link).copied()
    }

    fn announce(&self) -> Packet {
        let neighbors: Vec<PeerId> = self
            .peers
            .iter()
            .filter(|(_, p)| !p.links.is_empty())
            .map(|(id, _)| *id)
            .collect();
        self.me.announce_packet(&self.nickname, neighbors)
    }

    pub fn on_link_up(&mut self, link: LinkId) -> Vec<Effect> {
        self.links.insert(link);
        vec![Effect::Send { packet: self.announce(), target: Target::Link(link), delay: LINK_ANNOUNCE_DELAY }]
    }

    pub fn on_link_down(&mut self, link: LinkId, now: u64) -> Vec<Effect> {
        self.links.remove(&link);
        let Some(peer_id) = self.link_peer.remove(&link) else {
            return Vec::new();
        };
        if let Some(peer) = self.peers.get_mut(&peer_id) {
            peer.links.remove(&link);
            if peer.links.is_empty() {
                peer.unlinked_at = Some(now);
            }
        }
        vec![self.peers_event()]
    }

    /// Periodic housekeeping; call about once a second.
    pub fn tick(&mut self, now: u64, rng: &mut impl Rng) -> Vec<Effect> {
        let mut out = Vec::new();
        self.seen.prune(now);
        self.reassembler.expire(now);
        while self.pending.front().is_some_and(|(_, _, at)| now.saturating_sub(*at) > PENDING_MS) {
            self.pending.pop_front();
        }

        let before = self.peers.len();
        self.peers.retain(|_, p| {
            if !p.links.is_empty() {
                return true;
            }
            let silent = now.saturating_sub(p.last_seen);
            let link_lost = p.unlinked_at.is_some_and(|at| now.saturating_sub(at) > DISCONNECT_GRACE_MS);
            silent <= PEER_STALE_MS && !(link_lost && silent > DISCONNECT_GRACE_MS)
        });
        if self.peers.len() != before {
            let peers = &self.peers;
            self.link_peer.retain(|_, id| peers.contains_key(id));
            out.push(self.peers_event());
        }

        if !self.links.is_empty() && now >= self.next_announce {
            out.push(Effect::Send { packet: self.announce(), target: Target::All, delay: Duration::ZERO });
            self.next_announce = now + ANNOUNCE_INTERVAL_MS + rng.gen_range(0..ANNOUNCE_JITTER_MS);
        }

        self.sync_announces.retain(|_, p| now.saturating_sub(p.timestamp) <= PEER_STALE_MS);
        // Backfilled packets can be older than ones already stored, so the
        // store isn't time-ordered: prune by age everywhere, not from the front.
        let ids = &mut self.sync_ids;
        self.sync_msgs.retain(|p| {
            let keep = now.saturating_sub(p.timestamp) <= MESSAGE_MAX_AGE_MS;
            if !keep {
                ids.remove(&packet_id(p));
            }
            keep
        });
        self.sync_replied.retain(|_, at| now.saturating_sub(*at) <= SYNC_REPLY_COOLDOWN_MS);
        if !self.links.is_empty() && now >= self.next_sync {
            if self.next_sync != 0 {
                out.push(Effect::Send { packet: self.sync_request(now), target: Target::All, delay: Duration::ZERO });
            }
            self.next_sync = now + SYNC_INTERVAL_MS + rng.gen_range(0..ANNOUNCE_JITTER_MS);
        }
        out
    }

    /// A REQUEST_SYNC listing what we hold, newest first. Neighbor-only
    /// (TTL 0) and signed, as iOS requires.
    fn sync_request(&self, now: u64) -> Packet {
        let mut known: Vec<&Packet> = self.sync_announces.values().chain(self.sync_msgs.iter()).collect();
        known.sort_by_key(|p| std::cmp::Reverse(p.timestamp));
        let entries: Vec<([u8; 16], u64)> = known.iter().map(|p| (packet_id(p), p.timestamp)).collect();
        let req = gsync::build_request(&entries, gsync::DEFAULT_FILTER_BYTES);
        let mut packet = Packet::new(MessageType::RequestSync, self.my_id, req.encode());
        packet.ttl = 0;
        packet.timestamp = now;
        self.me.sign(&mut packet);
        packet
    }

    fn store_for_sync(&mut self, packet: &Packet) {
        if packet.ptype == MessageType::Announce as u8 {
            let newer = self.sync_announces.get(&packet.sender).is_none_or(|old| old.timestamp < packet.timestamp);
            if newer {
                self.sync_announces.insert(packet.sender, packet.clone());
            }
            return;
        }
        if !self.sync_ids.insert(packet_id(packet)) {
            return;
        }
        self.sync_msgs.push_back(packet.clone());
        while self.sync_msgs.len() > SYNC_STORE_MAX {
            if let Some(old) = self.sync_msgs.pop_front() {
                self.sync_ids.remove(&packet_id(&old));
            }
        }
    }

    /// Answer a neighbor's REQUEST_SYNC with what its filter lacks, sent
    /// back on the same link with TTL 0 and the RSR flag (iOS drops old
    /// packets without it).
    fn answer_sync(&mut self, link: LinkId, packet: &Packet, now: u64, out: &mut Vec<Effect>) {
        // Only from the neighbor on this link, signed by its known key.
        if self.link_peer.get(&link) != Some(&packet.sender) {
            return;
        }
        let Some(key) = self.peers.get(&packet.sender).map(|p| p.signing_key) else {
            return;
        };
        if !identity::verify(packet, &key) {
            return;
        }
        if self.sync_replied.get(&packet.sender).is_some_and(|at| now.saturating_sub(*at) < SYNC_REPLY_COOLDOWN_MS) {
            return;
        }
        let Some(req) = RequestSync::decode(&packet.payload) else {
            return;
        };
        // iOS also sends fragment- and file-only requests on their own
        // schedule; those mustn't use up the cooldown for the next one.
        if !req.wants(gsync::TYPE_ANNOUNCE) && !req.wants(gsync::TYPE_MESSAGE) {
            return;
        }
        self.sync_replied.insert(packet.sender, now);
        let filter = req.filter();

        let mut missing: Vec<&Packet> = Vec::new();
        if req.wants(gsync::TYPE_ANNOUNCE) {
            missing.extend(self.sync_announces.values().filter(|p| p.sender != packet.sender));
        }
        if req.wants(gsync::TYPE_MESSAGE) {
            // Older than the requester's cursor is outside its filter, not
            // missing. Announces are exempt, as on iOS.
            missing.extend(self.sync_msgs.iter().filter(|p| req.since.is_none_or(|since| p.timestamp >= since)));
        }
        missing.retain(|p| !filter.might_contain(&packet_id(p)));
        // Announces first so the messages after them verify.
        missing.sort_by_key(|p| (p.ptype != MessageType::Announce as u8, p.timestamp));

        for (i, p) in missing.into_iter().take(SYNC_REPLY_MAX).enumerate() {
            let mut reply = p.clone();
            reply.ttl = 0;
            reply.rsr = true;
            out.push(Effect::Send {
                packet: reply,
                target: Target::Link(link),
                delay: Duration::from_millis(i as u64 * SYNC_REPLY_PACING_MS),
            });
        }
    }

    pub fn send_text(&mut self, text: &str, now: u64) -> Vec<Effect> {
        let text = text.trim();
        if text.is_empty() {
            return Vec::new();
        }
        let mut packet = self.me.message_packet(text);
        packet.timestamp = now;
        self.me.sign(&mut packet);
        self.seen.insert(packet_id(&packet), now);
        self.store_for_sync(&packet);
        let msg = ChatMessage {
            id: hex(&packet_id(&packet)),
            sender_id: self.my_id.hex(),
            nickname: self.nickname.clone(),
            text: text.to_owned(),
            timestamp: now,
            mine: true,
        };
        self.push_log(msg.clone());
        vec![
            Effect::Send { packet, target: Target::All, delay: Duration::ZERO },
            Effect::Event(Event::Message(msg)),
        ]
    }

    pub fn set_nickname(&mut self, nick: &str) -> Result<Vec<Effect>, &'static str> {
        let nick = sanitize_nickname(nick).ok_or("nickname must be 1-15 visible characters")?;
        self.nickname = nick;
        let mut out = vec![Effect::Event(Event::Identity(self.me()))];
        if !self.links.is_empty() {
            out.push(Effect::Send { packet: self.announce(), target: Target::All, delay: Duration::ZERO });
        }
        Ok(out)
    }

    /// Clear the chat. Messages from before now stay cleared even if sync
    /// delivers them again.
    pub fn clear_history(&mut self, now: u64) {
        self.log.clear();
        self.logged_ids.clear();
        self.cleared_before = now;
    }

    pub fn leave(&self) -> Packet {
        self.me.leave_packet()
    }

    /// Handle one frame received on `link`.
    pub fn on_frame(&mut self, link: LinkId, frame: &[u8], now: u64, rng: &mut impl Rng) -> Vec<Effect> {
        let Some(packet) = Packet::decode(frame) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        self.on_packet(link, packet, now, rng, &mut out);
        out
    }

    fn on_packet(&mut self, link: LinkId, packet: Packet, now: u64, rng: &mut impl Rng, out: &mut Vec<Effect>) {
        if packet.sender == self.my_id {
            return;
        }
        if packet.timestamp > now + FUTURE_SKEW_MS {
            return;
        }
        let age = now.saturating_sub(packet.timestamp);
        let ptype = MessageType::from_u8(packet.ptype);
        let id = packet_id(&packet);
        let is_direct_announce = ptype == Some(MessageType::Announce) && packet.ttl == MAX_TTL;
        if self.seen.contains(&id) && !is_direct_announce {
            return;
        }

        match ptype {
            Some(MessageType::Fragment) => {
                self.seen.insert(id, now);
                self.maybe_relay(link, &packet, age, rng, out);
                if let Some(whole) = self.reassembler.push(&packet, now) {
                    self.on_packet(link, whole, now, rng, out);
                }
            }
            Some(MessageType::Announce) => {
                if age > ANNOUNCE_MAX_SKEW_MS {
                    return;
                }
                if !self.handle_announce(link, &packet, now, out) {
                    return;
                }
                self.store_for_sync(&packet);
                let first = !self.seen.contains(&id);
                self.seen.insert(id, now);
                if first {
                    self.maybe_relay(link, &packet, age, rng, out);
                }
                self.drain_pending(packet.sender, now, rng, out);
            }
            Some(MessageType::Message) => {
                if age > MESSAGE_MAX_AGE_MS {
                    return;
                }
                let Some(key) = self.peers.get(&packet.sender).map(|p| p.signing_key) else {
                    self.hold(packet, link, now);
                    return;
                };
                if !identity::verify(&packet, &key) {
                    return;
                }
                self.seen.insert(id, now);
                self.touch(packet.sender, now);
                if packet.is_broadcast() {
                    self.handle_public(&packet, id, out);
                    self.store_for_sync(&packet);
                }
                self.maybe_relay(link, &packet, age, rng, out);
            }
            Some(MessageType::Leave) => {
                let Some(key) = self.peers.get(&packet.sender).map(|p| p.signing_key) else {
                    return;
                };
                if age > 300_000 || !identity::verify(&packet, &key) {
                    return;
                }
                self.seen.insert(id, now);
                // A LEAVE with a payload leaves a channel, not the mesh.
                if packet.payload.is_empty() && self.peers.remove(&packet.sender).is_some() {
                    self.link_peer.retain(|_, p| *p != packet.sender);
                    out.push(self.peers_event());
                }
                self.maybe_relay(link, &packet, age, rng, out);
            }
            Some(MessageType::FileTransfer | MessageType::VoiceFrame) => {
                // Signed like public messages; we don't render them, but we
                // only carry what verifies, as Android does.
                let Some(key) = self.peers.get(&packet.sender).map(|p| p.signing_key) else {
                    return;
                };
                if !identity::verify(&packet, &key) {
                    return;
                }
                self.seen.insert(id, now);
                self.maybe_relay(link, &packet, age, rng, out);
            }
            Some(MessageType::RequestSync) => {
                // Neighbor-only: never relayed, never recorded as seen (a
                // peer asks again with the same payload).
                self.answer_sync(link, &packet, now, out);
            }
            _ => {
                // Noise traffic and types we don't speak: carry them for
                // others, never consume them.
                self.seen.insert(id, now);
                self.touch(packet.sender, now);
                self.maybe_relay(link, &packet, age, rng, out);
            }
        }
    }

    /// Validate and apply an announce. Returns false to drop it.
    fn handle_announce(&mut self, link: LinkId, packet: &Packet, now: u64, out: &mut Vec<Effect>) -> bool {
        let Some(ann) = identity::verify_announce(packet) else {
            return false;
        };
        let (Some(noise), Some(signing)) = (ann.noise_key(), ann.signing_key()) else {
            return false;
        };
        let nickname = sanitize_nickname(&ann.nickname).unwrap_or_else(|| format!("anon{}", &packet.sender.hex()[..4]));
        let direct = packet.ttl == MAX_TTL;

        let mut changed = false;
        match self.peers.get_mut(&packet.sender) {
            Some(peer) => {
                // A different signing key for a known peer is an impostor.
                if peer.signing_key != signing || peer.noise_key != noise {
                    return false;
                }
                if peer.nickname != nickname {
                    peer.nickname = nickname;
                    changed = true;
                }
                peer.last_seen = now;
            }
            None => {
                self.peers.insert(
                    packet.sender,
                    Peer {
                        nickname,
                        noise_key: noise,
                        signing_key: signing,
                        last_seen: now,
                        links: HashSet::new(),
                        unlinked_at: None,
                    },
                );
                changed = true;
            }
        }

        if direct && self.links.contains(&link) {
            if let Some(old) = self.link_peer.insert(link, packet.sender)
                && old != packet.sender
                && let Some(p) = self.peers.get_mut(&old)
            {
                p.links.remove(&link);
                if p.links.is_empty() {
                    p.unlinked_at = Some(now);
                }
            }
            let peer = self.peers.get_mut(&packet.sender).expect("inserted above");
            let new_link = peer.links.insert(link);
            peer.unlinked_at = None;
            if new_link {
                changed = true;
                // A new neighbor: ask what we missed while apart.
                out.push(Effect::Send { packet: self.sync_request(now), target: Target::Link(link), delay: SYNC_FIRST_DELAY });
            }
        }
        if changed {
            out.push(self.peers_event());
        }
        true
    }

    fn handle_public(&mut self, packet: &Packet, id: [u8; 16], out: &mut Vec<Effect>) {
        if packet.timestamp <= self.cleared_before {
            return;
        }
        let Ok(text) = String::from_utf8(packet.payload.clone()) else {
            return;
        };
        let nickname = self
            .peers
            .get(&packet.sender)
            .map(|p| p.nickname.clone())
            .unwrap_or_else(|| format!("anon{}", &packet.sender.hex()[..4]));
        let msg = ChatMessage {
            id: hex(&id),
            sender_id: packet.sender.hex(),
            nickname,
            text,
            timestamp: packet.timestamp,
            mine: false,
        };
        if self.push_log(msg.clone()) {
            out.push(Effect::Event(Event::Message(msg)));
        }
    }

    fn maybe_relay(&mut self, link: LinkId, packet: &Packet, age: u64, rng: &mut impl Rng, out: &mut Vec<Effect>) {
        if age > RELAY_MAX_AGE_MS {
            return;
        }
        match relay::decide(packet, self.my_id, self.peers.len(), rng) {
            RelayDecision::Drop => {}
            RelayDecision::NextHop(next, p) => {
                let direct = self
                    .peers
                    .get(&next)
                    .and_then(|peer| peer.links.iter().copied().find(|l| *l != link));
                let target = direct.map_or(Target::AllExcept(link), Target::Link);
                out.push(Effect::Send { packet: p, target, delay: Duration::ZERO });
            }
            RelayDecision::Flood(p, delay) => {
                if self.links.len() > 1 || !self.links.contains(&link) {
                    out.push(Effect::Send { packet: p, target: Target::AllExcept(link), delay });
                }
            }
        }
    }

    fn hold(&mut self, packet: Packet, link: LinkId, now: u64) {
        if self.pending.len() >= PENDING_MAX {
            self.pending.pop_front();
        }
        self.pending.push_back((packet, link, now));
    }

    fn drain_pending(&mut self, sender: PeerId, now: u64, rng: &mut impl Rng, out: &mut Vec<Effect>) {
        let (ready, rest): (VecDeque<_>, VecDeque<_>) = self.pending.drain(..).partition(|(p, _, _)| p.sender == sender);
        self.pending = rest;
        for (packet, link, _) in ready {
            self.on_packet(link, packet, now, rng, out);
        }
    }

    fn touch(&mut self, id: PeerId, now: u64) {
        if let Some(p) = self.peers.get_mut(&id) {
            p.last_seen = now;
        }
    }

    /// Append to the log unless it's a duplicate; keeps the log ordered by
    /// timestamp and bounded.
    fn push_log(&mut self, msg: ChatMessage) -> bool {
        if !self.logged_ids.insert(msg.id.clone()) {
            return false;
        }
        let pos = self.log.iter().rposition(|m| m.timestamp <= msg.timestamp).map_or(0, |i| i + 1);
        self.log.insert(pos, msg);
        while self.log.len() > LOG_MAX {
            if let Some(old) = self.log.pop_front() {
                self.logged_ids.remove(&old.id);
            }
        }
        true
    }

    fn peers_event(&self) -> Effect {
        Effect::Event(Event::Peers(self.peers()))
    }
}

/// Trim, drop control characters, and cap at 15 characters like the apps.
pub fn sanitize_nickname(nick: &str) -> Option<String> {
    let clean: String = nick
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .chars()
        .take(NICKNAME_MAX)
        .collect();
    (!clean.is_empty()).then_some(clean)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitchat_proto::fragment;
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    const NOW: u64 = 1_750_000_000_000;

    fn rng() -> StdRng {
        StdRng::seed_from_u64(42)
    }

    fn node(nick: &str) -> Mesh {
        Mesh::new(Identity::generate(), nick.into(), Vec::new())
    }

    fn sends(effects: &[Effect]) -> Vec<(&Packet, &Target)> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::Send { packet, target, .. } => Some((packet, target)),
                _ => None,
            })
            .collect()
    }

    fn messages(effects: &[Effect]) -> Vec<&ChatMessage> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::Event(Event::Message(m)) => Some(m),
                _ => None,
            })
            .collect()
    }

    fn wire(p: &Packet) -> Vec<u8> {
        p.encode_for_ble().unwrap()
    }

    /// Link `b` to `a` on link 1 (a's side) and deliver b's announce.
    fn introduce(a: &mut Mesh, b: &Mesh, link: LinkId) {
        a.on_link_up(link);
        let mut ann = b.announce();
        ann.timestamp = NOW;
        b.me.sign(&mut ann);
        a.on_frame(link, &wire(&ann), NOW, &mut rng());
    }

    #[test]
    fn link_up_announces_to_that_link() {
        let mut a = node("alice");
        let fx = a.on_link_up(7);
        let s = sends(&fx);
        assert_eq!(s.len(), 1);
        assert_eq!(*s[0].1, Target::Link(7));
        assert!(identity::verify_announce(s[0].0).is_some());
    }

    #[test]
    fn direct_announce_binds_link_and_lists_peer() {
        let mut a = node("alice");
        let b = node("bob");
        introduce(&mut a, &b, 1);
        let peers = a.peers();
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].nickname, "bob");
        assert!(peers[0].direct);
        assert_eq!(a.link_peer(1), Some(b.peer_id()));
    }

    #[test]
    fn public_message_is_verified_and_shown() {
        let mut a = node("alice");
        let mut b = node("bob");
        introduce(&mut a, &b, 1);
        let fx = b.send_text("hello mesh", NOW);
        let (packet, _) = sends(&fx)[0];
        let got = a.on_frame(1, &wire(packet), NOW + 5, &mut rng());
        let msgs = messages(&got);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].text, "hello mesh");
        assert_eq!(msgs[0].nickname, "bob");
        assert!(!msgs[0].mine);
        // Duplicate delivery (second link) shows nothing new.
        assert!(messages(&a.on_frame(1, &wire(packet), NOW + 6, &mut rng())).is_empty());
    }

    #[test]
    fn forged_message_is_dropped() {
        let mut a = node("alice");
        let mut b = node("bob");
        introduce(&mut a, &b, 1);
        let fx = b.send_text("hello", NOW);
        let mut packet = sends(&fx)[0].0.clone();
        packet.payload = b"goodbye".to_vec();
        assert!(messages(&a.on_frame(1, &wire(&packet), NOW, &mut rng())).is_empty());
    }

    #[test]
    fn message_before_announce_waits_for_it() {
        let mut a = node("alice");
        let mut b = node("bob");
        a.on_link_up(1);
        let fx = b.send_text("early", NOW);
        assert!(messages(&a.on_frame(1, &wire(sends(&fx)[0].0), NOW, &mut rng())).is_empty());
        let mut ann = b.announce();
        ann.timestamp = NOW;
        b.me.sign(&mut ann);
        let got = a.on_frame(1, &wire(&ann), NOW + 10, &mut rng());
        assert_eq!(messages(&got)[0].text, "early");
    }

    #[test]
    fn relays_between_two_links_with_lower_ttl() {
        let mut a = node("alice");
        let mut b = node("bob");
        let c = node("carol");
        introduce(&mut a, &b, 1);
        introduce(&mut a, &c, 2);
        let fx = b.send_text("pass it on", NOW);
        let got = a.on_frame(1, &wire(sends(&fx)[0].0), NOW, &mut rng());
        let relayed = sends(&got);
        assert_eq!(relayed.len(), 1);
        assert_eq!(*relayed[0].1, Target::AllExcept(1));
        assert_eq!(relayed[0].0.ttl, MAX_TTL - 1);
        // The relayed copy still verifies for the far side.
        assert!(identity::verify(relayed[0].0, &b.me.signing_public()));
    }

    #[test]
    fn stale_messages_shown_but_not_relayed() {
        let mut a = node("alice");
        let mut b = node("bob");
        let c = node("carol");
        introduce(&mut a, &b, 1);
        introduce(&mut a, &c, 2);
        let fx = b.send_text("old news", NOW - 3_600_000);
        let got = a.on_frame(1, &wire(sends(&fx)[0].0), NOW, &mut rng());
        assert_eq!(messages(&got).len(), 1);
        assert!(sends(&got).is_empty());
    }

    #[test]
    fn fragmented_message_reassembles() {
        let mut a = node("alice");
        let mut b = node("bob");
        introduce(&mut a, &b, 1);
        // Varied enough that compression can't fit it in one frame.
        let long: String = (0..400u32).map(|i| format!("{} ", (i * 7919) % 1000)).collect();
        let fx = b.send_text(&long, NOW);
        let frags = fragment::split(sends(&fx)[0].0, 185).unwrap();
        assert!(frags.len() > 1);
        let mut shown = Vec::new();
        for f in &frags {
            shown.extend(messages(&a.on_frame(1, &wire(f), NOW, &mut rng())).into_iter().cloned());
        }
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].text, long.trim());
    }

    #[test]
    fn leave_removes_peer() {
        let mut a = node("alice");
        let b = node("bob");
        introduce(&mut a, &b, 1);
        let mut leave = b.leave();
        leave.timestamp = NOW;
        b.me.sign(&mut leave);
        a.on_frame(1, &wire(&leave), NOW, &mut rng());
        assert!(a.peers().is_empty());
    }

    #[test]
    fn impostor_announce_with_new_key_rejected() {
        let mut a = node("alice");
        let b = node("bob");
        introduce(&mut a, &b, 1);
        // Same noise key, different signing key.
        let fake = Identity::from_secrets(b.me.noise_secret_bytes(), [9; 32]);
        let mut ann = fake.announce_packet("mallory", vec![]);
        ann.timestamp = NOW + 1;
        fake.sign(&mut ann);
        a.on_frame(1, &wire(&ann), NOW + 1, &mut rng());
        assert_eq!(a.peers()[0].nickname, "bob");
    }

    #[test]
    fn peers_expire_after_link_loss() {
        let mut a = node("alice");
        let b = node("bob");
        introduce(&mut a, &b, 1);
        a.on_link_down(1, NOW + 1_000);
        assert!(!a.peers()[0].direct);
        a.tick(NOW + 5_000, &mut rng());
        assert_eq!(a.peers().len(), 1);
        a.tick(NOW + 20_000, &mut rng());
        assert!(a.peers().is_empty());
    }

    #[test]
    fn relayed_peers_expire_when_stale() {
        let mut a = node("alice");
        let b = node("bob");
        a.on_link_up(1);
        let mut ann = b.announce();
        ann.timestamp = NOW;
        ann.ttl = 5; // relayed, not direct
        b.me.sign(&mut ann);
        a.on_frame(1, &wire(&ann), NOW, &mut rng());
        assert!(!a.peers()[0].direct);
        a.tick(NOW + PEER_STALE_MS - 1, &mut rng());
        assert_eq!(a.peers().len(), 1);
        a.tick(NOW + PEER_STALE_MS + 1, &mut rng());
        assert!(a.peers().is_empty());
    }

    fn of_type(effects: &[Effect], t: MessageType) -> usize {
        sends(effects).iter().filter(|(p, _)| p.ptype == t as u8).count()
    }

    #[test]
    fn periodic_announce_only_with_links() {
        let mut a = node("alice");
        assert!(sends(&a.tick(NOW, &mut rng())).is_empty());
        a.on_link_up(1);
        assert_eq!(of_type(&a.tick(NOW, &mut rng()), MessageType::Announce), 1);
        assert!(sends(&a.tick(NOW + 1_000, &mut rng())).is_empty());
        let later = a.tick(NOW + ANNOUNCE_INTERVAL_MS + ANNOUNCE_JITTER_MS, &mut rng());
        assert_eq!(of_type(&later, MessageType::Announce), 1);
        assert_eq!(of_type(&later, MessageType::RequestSync), 1);
    }

    /// Deliver everything `from` sends into `to` on `link`, returning what
    /// `to` produced.
    fn deliver(effects: &[Effect], to: &mut Mesh, link: LinkId, now: u64) -> Vec<Effect> {
        let mut got = Vec::new();
        for (p, _) in sends(effects) {
            got.extend(to.on_frame(link, &wire(p), now, &mut rng()));
        }
        got
    }

    #[test]
    fn new_neighbor_gets_a_sync_request() {
        let mut a = node("alice");
        let b = node("bob");
        a.on_link_up(1);
        let mut ann = b.announce();
        ann.timestamp = NOW;
        b.me.sign(&mut ann);
        let fx = a.on_frame(1, &wire(&ann), NOW, &mut rng());
        let reqs: Vec<_> = sends(&fx).into_iter().filter(|(p, _)| p.ptype == MessageType::RequestSync as u8).collect();
        assert_eq!(reqs.len(), 1);
        assert_eq!(*reqs[0].1, Target::Link(1));
        assert_eq!(reqs[0].0.ttl, 0);
        assert!(identity::verify(reqs[0].0, &a.me.signing_public()));
    }

    #[test]
    fn sync_backfills_missed_messages() {
        // Bob heard carol while alice was away; alice links to bob later.
        let mut bob = node("bob");
        let mut carol = node("carol");
        let mut alice = node("alice");
        let t = NOW;
        introduce(&mut bob, &carol, 1);
        let said = carol.send_text("you missed this", t - 60_000);
        deliver(&said, &mut bob, 1, t - 60_000);

        // Alice and bob meet on link 7 (bob's side) / 9 (alice's side).
        let mut b_ann = bob.announce();
        b_ann.timestamp = t;
        bob.me.sign(&mut b_ann);
        alice.on_link_up(9);
        let alice_fx = alice.on_frame(9, &wire(&b_ann), t, &mut rng());
        let mut a_ann = alice.announce();
        a_ann.timestamp = t;
        alice.me.sign(&mut a_ann);
        bob.on_link_up(7);
        bob.on_frame(7, &wire(&a_ann), t, &mut rng());

        // Alice's request reaches bob; bob answers on link 7 with RSR set.
        let request: Vec<&Packet> = sends(&alice_fx).into_iter()
            .filter(|(p, _)| p.ptype == MessageType::RequestSync as u8).map(|(p, _)| p).collect();
        let answer = bob.on_frame(7, &wire(request[0]), t, &mut rng());
        let replies = sends(&answer);
        assert!(replies.iter().all(|(p, tgt)| p.rsr && p.ttl == 0 && **tgt == Target::Link(7)));
        assert_eq!(replies.iter().filter(|(p, _)| p.ptype == MessageType::Message as u8).count(), 1);

        // Delivered to alice, the message shows up (carol's announce came too).
        let shown = deliver(&answer, &mut alice, 9, t);
        let msgs = messages(&shown);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].text, "you missed this");
        assert_eq!(msgs[0].nickname, "carol");

        // Asking again right away gets nothing (cooldown), and once alice
        // has it, her filter covers it.
        assert!(sends(&bob.on_frame(7, &wire(request[0]), t + 1, &mut rng())).is_empty());
        let again = alice.sync_request(t + 20_000);
        let answer2 = bob.on_frame(7, &wire(&again), t + 20_000, &mut rng());
        assert_eq!(sends(&answer2).iter().filter(|(p, _)| p.ptype == MessageType::Message as u8).count(), 0);
    }

    /// Bob holds `n` messages from carol at NOW - n..NOW; alice is linked
    /// to bob on link 7 (bob's side).
    fn sync_pair(n: u64) -> (Mesh, Mesh, Mesh) {
        let mut bob = node("bob");
        let mut carol = node("carol");
        let mut alice = node("alice");
        introduce(&mut bob, &carol, 1);
        for i in 0..n {
            let said = carol.send_text(&format!("m{i}"), NOW - n + i);
            deliver(&said, &mut bob, 1, NOW);
        }
        bob.on_link_up(7);
        let mut a_ann = alice.announce();
        a_ann.timestamp = NOW;
        alice.me.sign(&mut a_ann);
        bob.on_frame(7, &wire(&a_ann), NOW, &mut rng());
        alice.on_link_up(9);
        (alice, bob, carol)
    }

    #[test]
    fn since_cursor_limits_replies() {
        let (alice, mut bob, _) = sync_pair(5);
        let mut req = alice.sync_request(NOW);
        let mut r = RequestSync::decode(&req.payload).unwrap();
        r.since = Some(NOW - 2); // alice's filter only reaches back this far
        req.payload = r.encode();
        alice.me.sign(&mut req);
        let answer = bob.on_frame(7, &wire(&req), NOW, &mut rng());
        let msgs: Vec<_> = sends(&answer).into_iter().filter(|(p, _)| p.ptype == MessageType::Message as u8).collect();
        assert_eq!(msgs.len(), 2);
        assert!(msgs.iter().all(|(p, _)| p.timestamp >= NOW - 2));
    }

    #[test]
    fn sync_store_dedups_and_prunes_out_of_order() {
        let (_, mut bob, carol) = sync_pair(3);
        assert_eq!(bob.sync_msgs.len(), 3);
        // Re-delivery after the seen-set forgot it doesn't duplicate.
        let first = bob.sync_msgs[0].clone();
        bob.seen = SeenSet::new();
        bob.on_frame(1, &wire(&first), NOW, &mut rng());
        assert_eq!(bob.sync_msgs.len(), 3);
        // A very old backfilled packet behind newer ones still expires.
        let mut old = carol.me.message_packet("ancient");
        old.timestamp = NOW - MESSAGE_MAX_AGE_MS + 1_000;
        carol.me.sign(&mut old);
        bob.on_frame(1, &wire(&old), NOW, &mut rng());
        assert_eq!(bob.sync_msgs.len(), 4);
        bob.tick(NOW + 2_000, &mut rng());
        assert_eq!(bob.sync_msgs.len(), 3);
        assert_eq!(bob.sync_ids.len(), 3);
    }

    #[test]
    fn cleared_messages_stay_cleared() {
        let mut a = node("alice");
        let mut b = node("bob");
        introduce(&mut a, &b, 1);
        let said = b.send_text("before", NOW);
        deliver(&said, &mut a, 1, NOW);
        a.clear_history(NOW + 10);
        assert_eq!(a.messages().count(), 0);
        a.seen = SeenSet::new();
        assert!(messages(&deliver(&said, &mut a, 1, NOW + 20)).is_empty());
        let later = b.send_text("after", NOW + 30);
        assert_eq!(messages(&deliver(&later, &mut a, 1, NOW + 30)).len(), 1);
    }

    #[test]
    fn sync_request_from_stranger_is_ignored() {
        let mut bob = node("bob");
        let mallory = node("mallory");
        bob.on_link_up(7);
        let req = mallory.sync_request(bitchat_proto::now_ms());
        assert!(sends(&bob.on_frame(7, &wire(&req), bitchat_proto::now_ms(), &mut rng())).is_empty());
    }

    #[test]
    fn noise_traffic_is_carried_not_consumed() {
        let mut a = node("alice");
        let b = node("bob");
        let c = node("carol");
        introduce(&mut a, &b, 1);
        introduce(&mut a, &c, 2);
        let mut hs = Packet::new(MessageType::NoiseHandshake, b.peer_id(), vec![0; 32]);
        hs.recipient = Some(c.peer_id());
        hs.timestamp = NOW;
        let got = a.on_frame(1, &wire(&hs), NOW, &mut rng());
        let s = sends(&got);
        assert_eq!(s.len(), 1);
        assert_eq!(*s[0].1, Target::AllExcept(1));
        assert!(messages(&got).is_empty());
    }

    #[test]
    fn nickname_rules() {
        assert_eq!(sanitize_nickname("  raven  ").as_deref(), Some("raven"));
        assert_eq!(sanitize_nickname("a\u{7}b").as_deref(), Some("ab"));
        assert_eq!(sanitize_nickname("abcdefghijklmnopq").as_deref(), Some("abcdefghijklmno"));
        assert_eq!(sanitize_nickname("   "), None);
    }

    #[test]
    fn history_is_ordered_bounded_and_deduped() {
        let mut a = node("alice");
        for i in 0..(LOG_MAX + 10) as u64 {
            a.send_text(&format!("m{i}"), NOW + i);
        }
        assert_eq!(a.messages().count(), LOG_MAX);
        let msgs: Vec<_> = a.messages().collect();
        assert!(msgs.windows(2).all(|w| w[0].timestamp <= w[1].timestamp));
        let first = msgs[0].clone();
        assert!(!a.push_log(first));
    }
}
