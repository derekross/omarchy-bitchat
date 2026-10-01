//! The running node: the mesh engine plus persistence, the UI event bus and
//! the queue of frames for the radio.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bitchat_proto::Packet;
use rand::SeedableRng;
use rand::rngs::StdRng;
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc, watch};

use crate::mesh::{Effect, Event, LinkId, Mesh, Target};
use crate::power::Effective;
use crate::store::{Mode, Settings, Store};

/// Longest message we accept from the UI, in bytes. Fragmentation carries
/// it, but Android refuses sets above 256 fragments; this stays far below.
pub const MAX_TEXT_BYTES: usize = 4000;
/// Messages included in a snapshot.
pub const SNAPSHOT_MESSAGES: usize = 200;

pub struct Outgoing {
    pub packet: Packet,
    pub target: Target,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RadioState {
    /// Mode is off; the daemon holds no radio resources.
    Off,
    Starting,
    Running,
    /// The Bluetooth adapter is powered off or blocked.
    AdapterOff,
    NoAdapter,
    Error,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RadioStatus {
    pub state: RadioState,
    pub detail: Option<String>,
    pub effective: Effective,
    pub on_battery: bool,
    pub audio: bool,
    pub links: usize,
    pub advertising: bool,
    pub scanning: bool,
}

impl Default for RadioStatus {
    fn default() -> Self {
        RadioStatus {
            state: RadioState::Starting,
            detail: None,
            effective: Effective::Balanced,
            on_battery: false,
            audio: false,
            links: 0,
            advertising: false,
            scanning: false,
        }
    }
}

pub struct Node {
    mesh: Mutex<(Mesh, StdRng)>,
    store: Store,
    settings: Mutex<Settings>,
    events: broadcast::Sender<Value>,
    out: mpsc::UnboundedSender<Outgoing>,
    radio: watch::Sender<RadioStatus>,
    mode: watch::Sender<Mode>,
    last_compaction: Mutex<Option<std::time::Instant>>,
}

impl Node {
    pub fn new(mesh: Mesh, store: Store, settings: Settings) -> (Arc<Node>, mpsc::UnboundedReceiver<Outgoing>) {
        let (out, out_rx) = mpsc::unbounded_channel();
        let (events, _) = broadcast::channel(256);
        let node = Node {
            mesh: Mutex::new((mesh, StdRng::from_entropy())),
            mode: watch::Sender::new(settings.mode),
            settings: Mutex::new(settings),
            store,
            events,
            out,
            radio: watch::Sender::new(RadioStatus::default()),
            last_compaction: Mutex::new(None),
        };
        (Arc::new(node), out_rx)
    }

    pub fn events(&self) -> broadcast::Receiver<Value> {
        self.events.subscribe()
    }

    pub fn mode(&self) -> watch::Receiver<Mode> {
        self.mode.subscribe()
    }

    fn with_mesh<T>(&self, f: impl FnOnce(&mut Mesh, &mut StdRng) -> T) -> T {
        let mut guard = self.mesh.lock().unwrap_or_else(|e| e.into_inner());
        let (mesh, rng) = &mut *guard;
        f(mesh, rng)
    }

    pub fn link_peer(&self, link: LinkId) -> Option<bitchat_proto::PeerId> {
        self.with_mesh(|m, _| m.link_peer(link))
    }

    pub fn on_frame(&self, link: LinkId, frame: &[u8]) {
        let fx = self.with_mesh(|m, rng| m.on_frame(link, frame, bitchat_proto::now_ms(), rng));
        self.apply(fx);
    }

    pub fn on_link_up(&self, link: LinkId) {
        let (fx, n) = self.with_mesh(|m, _| (m.on_link_up(link), m.link_count()));
        self.set_radio(|r| r.links = n);
        self.apply(fx);
    }

    pub fn on_link_down(&self, link: LinkId) {
        let (fx, n) = self.with_mesh(|m, _| (m.on_link_down(link, bitchat_proto::now_ms()), m.link_count()));
        self.set_radio(|r| r.links = n);
        self.apply(fx);
    }

    pub fn tick(&self) {
        let (fx, pins) = self.with_mesh(|m, rng| (m.tick(bitchat_proto::now_ms(), rng), m.take_dirty_pins()));
        if let Some(pins) = pins
            && let Err(e) = self.store.save_pins(&pins)
        {
            tracing::warn!("saving pinned keys: {e:#}");
        }
        self.apply(fx);
    }

    pub fn send_text(&self, text: &str) -> Result<(), String> {
        if text.len() > MAX_TEXT_BYTES {
            return Err(format!("message is longer than {MAX_TEXT_BYTES} bytes"));
        }
        if text.trim().is_empty() {
            return Err("message is empty".into());
        }
        let fx = self.with_mesh(|m, _| m.send_text(text, bitchat_proto::now_ms()));
        self.apply(fx);
        Ok(())
    }

    pub fn set_nickname(&self, nick: &str) -> Result<(), String> {
        let fx = self.with_mesh(|m, _| {
            let fx = m.set_nickname(nick).map_err(str::to_owned)?;
            self.store
                .save_identity(m.identity(), m.nickname())
                .map_err(|e| format!("saving nickname: {e:#}"))?;
            Ok::<_, String>(fx)
        })?;
        self.apply(fx);
        Ok(())
    }

    pub fn set_mode(&self, mode: Mode) -> Result<(), String> {
        let mut s = self.settings.lock().unwrap_or_else(|e| e.into_inner());
        s.mode = mode;
        self.store.save_settings(&s).map_err(|e| e.to_string())?;
        drop(s);
        self.mode.send_replace(mode);
        self.emit_settings();
        Ok(())
    }

    pub fn set_persist_history(&self, enabled: bool) -> Result<(), String> {
        let mut s = self.settings.lock().unwrap_or_else(|e| e.into_inner());
        s.persist_history = enabled;
        self.store.save_settings(&s).map_err(|e| e.to_string())?;
        drop(s);
        if enabled {
            let msgs: Vec<_> = self.with_mesh(|m, _| m.messages().cloned().collect());
            self.store.rewrite_history(&msgs).map_err(|e| e.to_string())?;
        } else {
            self.store.clear_history().map_err(|e| e.to_string())?;
        }
        self.emit_settings();
        Ok(())
    }

    /// Forget a peer and its pinned signing key, so a new key for that peer
    /// ID is accepted (a phone that reset its keys, say).
    pub fn forget_peer(&self, id: &str) -> Result<bool, String> {
        let id = bitchat_proto::PeerId::from_hex(id).ok_or("peer id must be 16 hex digits")?;
        let (forgot, pins) = self.with_mesh(|m, _| (m.forget(id), m.take_dirty_pins()));
        if let Some(pins) = pins {
            self.store.save_pins(&pins).map_err(|e| e.to_string())?;
        }
        Ok(forgot)
    }

    pub fn clear_history(&self) -> Result<(), String> {
        self.with_mesh(|m, _| m.clear_history(bitchat_proto::now_ms()));
        self.store.clear_history().map_err(|e| e.to_string())?;
        let _ = self.events.send(json!({ "event": "cleared", "data": null }));
        Ok(())
    }

    /// The LEAVE we send on the way out.
    pub fn leave_packet(&self) -> Packet {
        self.with_mesh(|m, _| m.leave())
    }

    /// At most one history compaction a minute, whatever the traffic.
    fn compaction_due(&self) -> bool {
        let mut last = self.last_compaction.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|t: std::time::Instant| t.elapsed() < Duration::from_secs(60)) {
            return false;
        }
        *last = Some(std::time::Instant::now());
        true
    }

    pub fn radio_status(&self) -> RadioStatus {
        self.radio.borrow().clone()
    }

    /// Update the radio status, telling subscribers only if it changed.
    pub fn set_radio(&self, f: impl FnOnce(&mut RadioStatus)) {
        let changed = self.radio.send_if_modified(|r| {
            let before = r.clone();
            f(r);
            *r != before
        });
        if changed {
            let _ = self.events.send(json!({ "event": "status", "data": self.radio_status() }));
        }
    }

    fn emit_settings(&self) {
        let s = self.settings.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let _ = self.events.send(json!({ "event": "settings", "data": s }));
    }

    pub fn snapshot(&self) -> Value {
        let (me, peers, messages) = self.with_mesh(|m, _| {
            let all: Vec<_> = m.messages().cloned().collect();
            let recent = all[all.len().saturating_sub(SNAPSHOT_MESSAGES)..].to_vec();
            (m.me(), m.peers(), recent)
        });
        let settings = self.settings.lock().unwrap_or_else(|e| e.into_inner()).clone();
        json!({
            "me": me,
            "peers": peers,
            "messages": messages,
            "settings": settings,
            "radio": self.radio_status(),
            "version": env!("CARGO_PKG_VERSION"),
        })
    }

    pub fn apply(&self, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::Send { packet, target, delay } => {
                    if delay.is_zero() {
                        let _ = self.out.send(Outgoing { packet, target });
                    } else {
                        let out = self.out.clone();
                        tokio::spawn(async move {
                            tokio::time::sleep(delay).await;
                            let _ = out.send(Outgoing { packet, target });
                        });
                    }
                }
                Effect::Event(event) => {
                    if let Event::Message(msg) = &event {
                        let persist = self.settings.lock().unwrap_or_else(|e| e.into_inner()).persist_history;
                        if persist {
                            match self.store.append_history(msg) {
                                // Grown past its cap: rewrite it from the
                                // in-memory log (the last 500 messages).
                                Ok(true) if self.compaction_due() => {
                                    let msgs: Vec<_> = self.with_mesh(|m, _| m.messages().cloned().collect());
                                    if let Err(e) = self.store.rewrite_history(&msgs) {
                                        tracing::warn!("compacting history: {e:#}");
                                    }
                                }
                                Ok(_) => {}
                                Err(e) => tracing::warn!("saving history: {e:#}"),
                            }
                        }
                    }
                    if let Ok(v) = serde_json::to_value(&event) {
                        let _ = self.events.send(v);
                    }
                }
            }
        }
    }

    /// Queue a packet directly (the LEAVE on shutdown).
    pub fn send_raw(&self, packet: Packet, target: Target) {
        let _ = self.out.send(Outgoing { packet, target });
    }
}

/// Drive [`Node::tick`] once a second.
pub async fn ticker(node: Arc<Node>) {
    let mut every = tokio::time::interval(Duration::from_secs(1));
    loop {
        every.tick().await;
        node.tick();
    }
}
