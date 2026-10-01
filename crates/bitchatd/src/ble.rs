//! The Bluetooth side: BlueZ over D-Bus via `bluer`.
//!
//! The laptop plays both roles, like the phone apps:
//!
//! - **Peripheral.** We host the bitchat GATT service and advertise it.
//!   Phones connect to us, write frames to the characteristic, and get ours
//!   as notifications. Backgrounded iPhones can only reach us this way.
//! - **Central.** We scan for the service UUID in duty-cycled bursts,
//!   connect, write frames without response and subscribe to notifications.
//!
//! BlueZ sends a notification to every subscribed central at once, so the
//! peripheral side can't target or exclude one link. The mesh dedups, so
//! that only costs airtime. It also truncates each notification to that
//! central's MTU, so peripheral frames are sized to the smallest MTU seen.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use bitchat_proto::{CHARACTERISTIC_UUID, FRAGMENT_THRESHOLD, Packet, SERVICE_UUID, fragment};
use bluer::adv::{Advertisement, AdvertisementHandle, Type as AdvType};
use bluer::gatt::local::{
    Application, ApplicationHandle, Characteristic, CharacteristicNotifier, CharacteristicNotify,
    CharacteristicNotifyMethod, CharacteristicRead, CharacteristicWrite, CharacteristicWriteMethod, Service,
};
use bluer::gatt::remote;
use bluer::{Adapter, AdapterEvent, Address, DeviceEvent, DeviceProperty, DiscoveryFilter, DiscoveryTransport, Session, Uuid};
use futures::{FutureExt, Stream, StreamExt};
use tokio::sync::{Notify, mpsc, watch};
use tokio::task::JoinSet;

use crate::mesh::{LinkId, Target};
use crate::node::{Node, Outgoing, RadioState};
use crate::power::{self, Effective};
use crate::store::Mode;

pub const SERVICE: Uuid = Uuid::from_u128(SERVICE_UUID);
pub const CHARACTERISTIC: Uuid = Uuid::from_u128(CHARACTERISTIC_UUID);

const MAX_CENTRAL_LINKS: usize = 8;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const SETUP_TIMEOUT: Duration = Duration::from_secs(15);
/// Pause between frames of one fragmented packet (Android uses 20 ms).
const FRAME_PACING: Duration = Duration::from_millis(20);
const RSSI_FLOOR: i16 = -90;
/// Below this many bytes per write, fragment headers leave no room for data.
const MIN_FRAME: usize = 96;
/// How long a phone address stays off-limits after we closed a duplicate
/// link to it (the phone is already linked under another address).
const DUPLICATE_BACKOFF: Duration = Duration::from_secs(600);
/// Assumed for a central that hasn't told us its MTU yet (iOS's usual 185).
const DEFAULT_CENTRAL_MTU: usize = 185;
const HEALTH_INTERVAL: Duration = Duration::from_secs(10);
const MONITOR_INTERVAL: Duration = Duration::from_secs(3);
const POWER_INTERVAL: Duration = Duration::from_secs(15);
const CLEANUP_INTERVAL: Duration = Duration::from_secs(300);
const MAX_WRITE_BUFFER: usize = 64 * 1024;
/// Most notification frames waiting at once.
const MAX_NOTIFY_QUEUE: usize = 512;
/// A link whose far end hasn't announced itself by now is dropped: it's
/// holding a slot without taking part in the mesh.
const ANNOUNCE_DEADLINE: Duration = Duration::from_secs(20);
/// Retry a failed radio session after 5 s, doubling up to 5 minutes.
const RETRY_BASE: Duration = Duration::from_secs(5);
const RETRY_MAX: Duration = Duration::from_secs(300);
/// A session that lasted this long resets the backoff.
const STABLE_SESSION: Duration = Duration::from_secs(120);

#[derive(Debug)]
enum SessionEnd {
    /// Mode switched to off.
    Stopped,
    NoAdapter,
    AdapterOff,
    Failed(anyhow::Error),
}

enum LinkKind {
    Central {
        tx: mpsc::Sender<Vec<u8>>,
        max_frame: usize,
        close: Arc<Notify>,
        /// We opened the connection (versus riding one a phone opened).
        initiated: bool,
    },
    Peripheral {
        mtu: usize,
    },
}

struct Link {
    addr: Address,
    kind: LinkKind,
    since: Instant,
}

/// One live radio session: everything registered with BlueZ and every link.
/// Rebuilt from scratch after BlueZ restarts or the adapter comes back.
struct Radio {
    node: Arc<Node>,
    adapter: Adapter,
    links: Mutex<HashMap<LinkId, Link>>,
    connecting: Mutex<HashSet<Address>>,
    failures: Mutex<HashMap<Address, (u32, Instant)>>,
    /// Addresses we closed as duplicates; skipped until the instant passes.
    suppressed: Mutex<HashMap<Address, Instant>>,
    write_bufs: Mutex<HashMap<Address, Vec<u8>>>,
    notifier: tokio::sync::Mutex<Option<CharacteristicNotifier>>,
    /// Frames waiting for the notifier.
    notify_queued: Arc<std::sync::atomic::AtomicUsize>,
    tasks: Mutex<JoinSet<()>>,
}

/// Gives `n` back to a counter when dropped: when its task finishes, is
/// aborted, or is dropped before it ever ran.
struct Release(Arc<std::sync::atomic::AtomicUsize>, usize);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.fetch_sub(self.1, Ordering::Relaxed);
    }
}

static NEXT_LINK: AtomicU64 = AtomicU64::new(1);

impl Radio {
    fn new(node: Arc<Node>, adapter: Adapter) -> Arc<Radio> {
        Arc::new(Radio {
            node,
            adapter,
            links: Mutex::new(HashMap::new()),
            connecting: Mutex::new(HashSet::new()),
            failures: Mutex::new(HashMap::new()),
            suppressed: Mutex::new(HashMap::new()),
            write_bufs: Mutex::new(HashMap::new()),
            notifier: tokio::sync::Mutex::new(None),
            notify_queued: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            tasks: Mutex::new(JoinSet::new()),
        })
    }

    fn lock_links(&self) -> std::sync::MutexGuard<'_, HashMap<LinkId, Link>> {
        self.links.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn spawn(&self, fut: impl Future<Output = ()> + Send + 'static) {
        let mut tasks = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
        // A JoinSet keeps finished tasks until they're joined; reap them here
        // so a long session doesn't grow without bound.
        while tasks.try_join_next().is_some() {}
        tasks.spawn(fut);
    }

    fn has_link_to(&self, addr: Address) -> bool {
        self.lock_links().values().any(|l| l.addr == addr)
    }

    fn central_count(&self) -> usize {
        self.lock_links().values().filter(|l| matches!(l.kind, LinkKind::Central { .. })).count()
    }

    // ---------- sending ----------

    fn send(self: &Arc<Self>, out: Outgoing) {
        let mut central = Vec::new();
        let mut peripheral_targeted = false;
        let mut peripheral_min_mtu: Option<usize> = None;
        {
            let links = self.lock_links();
            for (&id, link) in links.iter() {
                // A central that never raised its MTU can't carry a frame at
                // all; it mustn't shrink notifications for everyone else.
                if let LinkKind::Peripheral { mtu } = link.kind
                    && mtu.saturating_sub(3) >= MIN_FRAME
                {
                    peripheral_min_mtu = Some(peripheral_min_mtu.map_or(mtu, |m: usize| m.min(mtu)));
                }
                let selected = match out.target {
                    Target::All => true,
                    Target::AllExcept(ex) => id != ex,
                    Target::Link(only) => id == only,
                };
                if !selected {
                    continue;
                }
                match &link.kind {
                    LinkKind::Central { tx, max_frame, .. } => central.push((tx.clone(), *max_frame)),
                    LinkKind::Peripheral { .. } => peripheral_targeted = true,
                }
            }
        }

        for (tx, max_frame) in central {
            for frame in frames(&out.packet, max_frame) {
                if tx.try_send(frame).is_err() {
                    tracing::debug!("central link queue full; dropping a frame");
                    break;
                }
            }
        }

        if peripheral_targeted {
            // BlueZ cuts each notification to the central's ATT MTU - 3.
            let max_frame = peripheral_min_mtu.map_or(FRAGMENT_THRESHOLD, |mtu| frame_limit(mtu.saturating_sub(3)));
            let frames = frames(&out.packet, max_frame);
            // Bounded: when the radio can't keep up (a flood), drop rather
            // than queue without limit.
            let queued = self.notify_queued.fetch_add(frames.len(), Ordering::Relaxed);
            if queued + frames.len() > MAX_NOTIFY_QUEUE {
                self.notify_queued.fetch_sub(frames.len(), Ordering::Relaxed);
                tracing::debug!("notify queue full; dropping a packet");
                return;
            }
            // Created before the task, so the count comes back even if the
            // task is aborted before its first poll.
            let release = Release(self.notify_queued.clone(), frames.len());
            let radio = self.clone();
            self.spawn(async move {
                let _release = release;
                let count = frames.len();
                let mut guard = radio.notifier.lock().await;
                let Some(notifier) = guard.as_mut() else {
                    return;
                };
                for (i, frame) in frames.into_iter().enumerate() {
                    if notifier.notify(frame).await.is_err() {
                        *guard = None;
                        return;
                    }
                    if i + 1 < count {
                        tokio::time::sleep(FRAME_PACING).await;
                    }
                }
            });
        }
    }

    // ---------- peripheral role ----------

    fn application(self: &Arc<Self>) -> Application {
        let on_write = self.clone();
        let on_notify = self.clone();
        Application {
            services: vec![Service {
                uuid: SERVICE,
                primary: true,
                characteristics: vec![Characteristic {
                    uuid: CHARACTERISTIC,
                    read: Some(CharacteristicRead {
                        read: true,
                        fun: Box::new(|_| async { Ok(Vec::new()) }.boxed()),
                        ..Default::default()
                    }),
                    write: Some(CharacteristicWrite {
                        write: true,
                        write_without_response: true,
                        method: CharacteristicWriteMethod::Fun(Box::new(move |value, req| {
                            let radio = on_write.clone();
                            async move {
                                radio.on_peripheral_write(req.device_address, value, req.offset as usize, req.mtu as usize);
                                Ok(())
                            }
                            .boxed()
                        })),
                        ..Default::default()
                    }),
                    notify: Some(CharacteristicNotify {
                        notify: true,
                        method: CharacteristicNotifyMethod::Fun(Box::new(move |notifier| {
                            let radio = on_notify.clone();
                            async move {
                                tracing::debug!("a central subscribed to notifications");
                                *radio.notifier.lock().await = Some(notifier);
                            }
                            .boxed()
                        })),
                        ..Default::default()
                    }),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn on_peripheral_write(&self, addr: Address, value: Vec<u8>, offset: usize, mtu: usize) {
        let link = self.peripheral_link(addr, mtu);

        // Long writes arrive in pieces with offsets; plain writes (what the
        // apps send) are one whole frame each.
        let frame = {
            let mut bufs = self.write_bufs.lock().unwrap_or_else(|e| e.into_inner());
            // Whole once the header's length is covered; the mesh decodes it.
            let whole = |b: &[u8]| Packet::frame_len(b).is_some_and(|n| b.len() >= n);
            if offset == 0 {
                if whole(&value) {
                    bufs.remove(&addr);
                    Some(value)
                } else {
                    bufs.insert(addr, value);
                    None
                }
            } else {
                let complete = match bufs.get_mut(&addr) {
                    Some(buf) if buf.len() == offset && buf.len() + value.len() <= MAX_WRITE_BUFFER => {
                        buf.extend_from_slice(&value);
                        whole(buf)
                    }
                    _ => {
                        bufs.remove(&addr);
                        false
                    }
                };
                if complete { bufs.remove(&addr) } else { None }
            }
        };
        if let Some(frame) = frame {
            self.node.on_frame(link, &frame);
        }
    }

    /// The peripheral link for `addr`, created (and announced to) on its
    /// first write.
    fn peripheral_link(&self, addr: Address, mtu: usize) -> LinkId {
        let mut links = self.lock_links();
        for (&id, link) in links.iter_mut() {
            if link.addr == addr
                && let LinkKind::Peripheral { mtu: m } = &mut link.kind {
                    if mtu > 0 {
                        *m = mtu;
                    }
                    return id;
                }
        }
        let id = NEXT_LINK.fetch_add(1, Ordering::Relaxed);
        let mtu = if mtu > 0 { mtu } else { DEFAULT_CENTRAL_MTU };
        links.insert(id, Link { addr, kind: LinkKind::Peripheral { mtu }, since: Instant::now() });
        drop(links);
        tracing::info!("{addr} connected to us (link {id}, mtu {mtu})");
        self.node.on_link_up(id);
        id
    }

    // ---------- central role ----------

    async fn scan_loop(self: Arc<Self>, mut effective: watch::Receiver<Effective>) {
        // LE only, and nothing else: BlueZ 5.87 segfaults in its
        // device-found path when a UUID-filtered discovery sees a matching
        // advertiser (reproduced with plain bluetoothctl). `consider` checks
        // the service UUID and RSSI itself instead.
        let filter = DiscoveryFilter {
            transport: DiscoveryTransport::Le,
            duplicate_data: false,
            ..Default::default()
        };
        if let Err(e) = self.adapter.set_discovery_filter(filter).await {
            tracing::warn!("setting discovery filter: {e}");
        }
        loop {
            let Some((on, off)) = effective.borrow().scan_cycle() else {
                return;
            };
            match self.adapter.discover_devices().await {
                Ok(stream) => {
                    let started = Instant::now();
                    self.node.set_radio(|r| r.scanning = true);
                    self.scan_burst(stream, on).await;
                    self.node.set_radio(|r| r.scanning = false);
                    tracing::debug!("scan burst ended after {:?}", started.elapsed());
                }
                Err(e) => tracing::warn!("starting discovery: {e}"),
            }
            // Phones that connected to us but that we aren't writing to yet.
            self.consider_connected().await;
            tokio::select! {
                _ = tokio::time::sleep(off) => {}
                _ = effective.changed() => {}
            }
        }
    }

    /// Consume discovery events for `duration`, then drop the stream, which
    /// stops discovery. A scan left running starves Bluetooth audio.
    async fn scan_burst(self: &Arc<Self>, stream: impl Stream<Item = AdapterEvent>, duration: Duration) {
        let deadline = tokio::time::sleep(duration);
        tokio::pin!(deadline, stream);
        loop {
            tokio::select! {
                ev = stream.next() => match ev {
                    Some(AdapterEvent::DeviceAdded(addr)) => {
                        // Off the select loop, so a crowded room can't
                        // stretch the burst past its deadline.
                        let radio = self.clone();
                        self.spawn(async move { radio.consider(addr, true).await });
                    }
                    Some(_) => {}
                    None => break,
                },
                _ = &mut deadline => break,
            }
        }
    }

    async fn consider_connected(self: &Arc<Self>) {
        let Ok(addrs) = self.adapter.device_addresses().await else {
            return;
        };
        for addr in addrs {
            let Ok(device) = self.adapter.device(addr) else { continue };
            if device.is_connected().await.unwrap_or(false) {
                self.consider(addr, false).await;
            }
        }
    }

    /// Maybe open a central link to `addr`. `in_range` demands a fresh RSSI:
    /// discovery first replays every device BlueZ remembers, including
    /// rotated-away phone addresses that would only stall the connect queue.
    async fn consider(self: &Arc<Self>, addr: Address, in_range: bool) {
        if self.has_link_to(addr) || self.central_count() >= MAX_CENTRAL_LINKS {
            return;
        }
        if let Some(until) = self.suppressed.lock().unwrap_or_else(|e| e.into_inner()).get(&addr)
            && Instant::now() < *until
        {
            return;
        }
        if let Some((_, until)) = self.failures.lock().unwrap_or_else(|e| e.into_inner()).get(&addr)
            && Instant::now() < *until {
                return;
            }
        let Ok(device) = self.adapter.device(addr) else { return };
        let has_service = device.uuids().await.ok().flatten().is_some_and(|u| u.contains(&SERVICE));
        if !has_service {
            return;
        }
        if in_range && !matches!(device.rssi().await, Ok(Some(rssi)) if rssi >= RSSI_FLOOR) {
            return;
        }
        if !self.connecting.lock().unwrap_or_else(|e| e.into_inner()).insert(addr) {
            return;
        }
        let radio = self.clone();
        self.spawn(async move {
            let result = radio.clone().central_link(addr).await;
            radio.connecting.lock().unwrap_or_else(|e| e.into_inner()).remove(&addr);
            let mut failures = radio.failures.lock().unwrap_or_else(|e| e.into_inner());
            match result {
                Ok(()) => {
                    failures.remove(&addr);
                }
                Err(e) => {
                    let n = failures.get(&addr).map_or(0, |(n, _)| *n) + 1;
                    // Three quick retries 5 s apart, then back off (address
                    // rotation makes a dead address common).
                    let wait = if n < 3 { Duration::from_secs(5) } else { Duration::from_secs(120) };
                    failures.insert(addr, (n, Instant::now() + wait));
                    tracing::debug!("central link to {addr} failed ({n}): {e:#}");
                }
            }
        });
    }

    /// Connect (unless already connected), find the characteristic, and pump
    /// frames both ways until the link drops.
    async fn central_link(self: Arc<Self>, addr: Address) -> Result<()> {
        let device = self.adapter.device(addr)?;
        let was_connected = device.is_connected().await.unwrap_or(false);
        if !was_connected {
            let attempt = tokio::time::timeout(CONNECT_TIMEOUT, device.connect()).await;
            if !matches!(attempt, Ok(Ok(()))) {
                // Dropping the call doesn't stop BlueZ's LE connect attempt,
                // which would hold up connects to phones that are in range.
                let _ = device.disconnect().await;
                return Err(match attempt {
                    Ok(Err(e)) => anyhow::Error::from(e).context("connect"),
                    _ => anyhow!("connect timed out"),
                });
            }
        }
        let setup = async {
            let chr = find_characteristic(&device).await?;
            let notes = Notifications::open(&chr).await?;
            let writer = chr.write_io().await.context("acquiring write")?;
            Ok::<_, anyhow::Error>((notes, writer))
        };
        let (mut notes, writer) = match tokio::time::timeout(SETUP_TIMEOUT, setup).await {
            Ok(Ok(v)) => v,
            other => {
                if !was_connected {
                    let _ = device.disconnect().await;
                }
                return Err(match other {
                    Ok(Err(e)) => e,
                    _ => anyhow!("service setup timed out"),
                });
            }
        };

        // bluer's writer MTU already leaves room for the ATT header.
        let max_frame = writer.mtu().min(FRAGMENT_THRESHOLD);
        if max_frame < MIN_FRAME {
            if !was_connected {
                let _ = device.disconnect().await;
            }
            return Err(anyhow!("link MTU too small ({max_frame} bytes per write)"));
        }
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(128);
        let close = Arc::new(Notify::new());
        let id = NEXT_LINK.fetch_add(1, Ordering::Relaxed);
        self.lock_links().insert(
            id,
            Link {
                addr,
                kind: LinkKind::Central { tx, max_frame, close: close.clone(), initiated: !was_connected },
                since: Instant::now(),
            },
        );
        tracing::info!("linked to {addr} as central (link {id}, frame {max_frame})");
        self.node.on_link_up(id);

        // Writes run in their own task so a long burst (a sync reply, say)
        // never stops us reading notifications.
        let mut pump = tokio::spawn(async move {
            while let Some(frame) = rx.recv().await {
                if writer.send(&frame).await.is_err() {
                    break;
                }
                if !rx.is_empty() {
                    tokio::time::sleep(FRAME_PACING).await;
                }
            }
        });

        let mut events = device.events().await.ok();
        loop {
            tokio::select! {
                frame = notes.next() => match frame {
                    Some(frame) => self.node.on_frame(id, &frame),
                    None => break,
                },
                _ = &mut pump => break,
                ev = next_event(&mut events) => {
                    if matches!(ev, Some(DeviceEvent::PropertyChanged(DeviceProperty::Connected(false))) | None) {
                        break;
                    }
                }
                _ = close.notified() => break,
            }
        }

        pump.abort();
        self.lock_links().remove(&id);
        self.node.on_link_down(id);
        tracing::info!("central link {id} to {addr} closed");
        if !was_connected {
            let _ = device.disconnect().await;
        }
        Ok(())
    }

    // ---------- housekeeping ----------

    /// Notice peripheral links whose central went away, and close duplicate
    /// central links to one peer (phones rotate addresses, so we can end up
    /// connected to the same phone twice).
    async fn monitor_loop(self: Arc<Self>) {
        let mut last_cleanup = Instant::now();
        loop {
            tokio::time::sleep(MONITOR_INTERVAL).await;

            let peripherals: Vec<(LinkId, Address)> = self
                .lock_links()
                .iter()
                .filter(|(_, l)| matches!(l.kind, LinkKind::Peripheral { .. }))
                .map(|(&id, l)| (id, l.addr))
                .collect();
            for (id, addr) in peripherals {
                let connected = match self.adapter.device(addr) {
                    Ok(d) => d.is_connected().await.unwrap_or(false),
                    Err(_) => false,
                };
                if !connected {
                    self.lock_links().remove(&id);
                    self.write_bufs.lock().unwrap_or_else(|e| e.into_inner()).remove(&addr);
                    tracing::info!("{addr} disconnected from us (link {id})");
                    self.node.on_link_down(id);
                }
            }

            // Links that never produced a valid announce.
            let silent: Vec<(Address, Arc<Notify>)> = self
                .lock_links()
                .iter()
                .filter(|(id, l)| l.since.elapsed() > ANNOUNCE_DEADLINE && self.node.link_peer(**id).is_none())
                .filter_map(|(_, l)| match &l.kind {
                    LinkKind::Central { close, .. } => Some((l.addr, close.clone())),
                    LinkKind::Peripheral { .. } => None,
                })
                .collect();
            for (addr, close) in silent {
                tracing::info!("{addr} never announced itself; dropping the link");
                self.suppressed
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(addr, Instant::now() + DUPLICATE_BACKOFF);
                close.notify_one();
            }
            self.failures
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .retain(|_, (_, until)| Instant::now() < *until + Duration::from_secs(600));

            let mut by_peer: HashMap<bitchat_proto::PeerId, Vec<(Instant, Address, Arc<Notify>)>> = HashMap::new();
            for (&id, link) in self.lock_links().iter() {
                if let (LinkKind::Central { close, .. }, Some(peer)) = (&link.kind, self.node.link_peer(id)) {
                    by_peer.entry(peer).or_default().push((link.since, link.addr, close.clone()));
                }
            }
            for (_, mut dupes) in by_peer {
                dupes.sort_by_key(|(since, _, _)| *since);
                for (_, addr, close) in dupes.into_iter().skip(1) {
                    // Don't reconnect to it on the next scan burst.
                    self.suppressed
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(addr, Instant::now() + DUPLICATE_BACKOFF);
                    close.notify_one();
                }
            }
            self.suppressed.lock().unwrap_or_else(|e| e.into_inner()).retain(|_, until| Instant::now() < *until);

            if last_cleanup.elapsed() >= CLEANUP_INTERVAL {
                last_cleanup = Instant::now();
                self.forget_stale_devices().await;
            }
        }
    }

    /// Phones rotate their address every ~15 minutes; each old address stays
    /// in BlueZ's device list. Remove unconnected, unpaired bitchat devices.
    async fn forget_stale_devices(&self) {
        let Ok(addrs) = self.adapter.device_addresses().await else { return };
        for addr in addrs {
            let Ok(d) = self.adapter.device(addr) else { continue };
            let ours = d.uuids().await.ok().flatten().is_some_and(|u| u.contains(&SERVICE));
            let busy = d.is_connected().await.unwrap_or(true)
                || d.is_paired().await.unwrap_or(true)
                || d.is_trusted().await.unwrap_or(true);
            if ours && !busy {
                let _ = self.adapter.remove_device(addr).await;
            }
        }
    }

    /// Tear the session down: every link goes down in the mesh, and
    /// connections we opened are closed.
    async fn shutdown(&self) {
        self.tasks.lock().unwrap_or_else(|e| e.into_inner()).abort_all();
        let links: Vec<(LinkId, Link)> = self.lock_links().drain().collect();
        for (id, link) in links {
            self.node.on_link_down(id);
            if let LinkKind::Central { initiated: true, .. } = link.kind
                && let Ok(d) = self.adapter.device(link.addr) {
                    let _ = d.disconnect().await;
                }
        }
        *self.notifier.lock().await = None;
    }
}

/// Frames for one packet on a link that carries at most `max_frame` bytes
/// per write or notification: fragmented if needed, padded where the
/// protocol pads and the padding still fits.
fn frames(packet: &Packet, max_frame: usize) -> Vec<Vec<u8>> {
    let Some(parts) = fragment::split(packet, max_frame) else {
        tracing::warn!("could not fit a packet into {max_frame}-byte frames");
        return Vec::new();
    };
    parts
        .iter()
        .filter_map(|p| {
            let padded = p.encode_for_ble().ok()?;
            if padded.len() <= max_frame { Some(padded) } else { p.encode(false).ok() }
        })
        .collect()
}

/// Frame size for a link carrying `payload` bytes per write/notification,
/// capped at the frame size both apps expect. Never above what the link
/// carries: BlueZ would cut the frame short.
fn frame_limit(payload: usize) -> usize {
    payload.min(FRAGMENT_THRESHOLD)
}

async fn find_characteristic(device: &bluer::Device) -> Result<remote::Characteristic> {
    for service in device.services().await.context("discovering services")? {
        if service.uuid().await? != SERVICE {
            continue;
        }
        for chr in service.characteristics().await? {
            if chr.uuid().await? == CHARACTERISTIC {
                return Ok(chr);
            }
        }
    }
    Err(anyhow!("no bitchat characteristic"))
}

async fn next_event(events: &mut Option<impl Stream<Item = DeviceEvent> + Unpin>) -> Option<DeviceEvent> {
    match events {
        Some(s) => s.next().await,
        None => std::future::pending().await,
    }
}

/// Incoming notifications: the low-overhead socket when BlueZ grants one,
/// otherwise D-Bus property changes.
enum Notifications {
    Io(bluer::gatt::CharacteristicReader),
    Stream(std::pin::Pin<Box<dyn Stream<Item = Vec<u8>> + Send>>),
}

impl Notifications {
    async fn open(chr: &remote::Characteristic) -> Result<Notifications> {
        match chr.notify_io().await {
            Ok(reader) => Ok(Notifications::Io(reader)),
            Err(e) => {
                tracing::debug!("notify_io unavailable ({e}); using D-Bus notifications");
                Ok(Notifications::Stream(Box::pin(chr.notify().await.context("subscribing")?)))
            }
        }
    }

    async fn next(&mut self) -> Option<Vec<u8>> {
        match self {
            // An empty read on the notify socket is EOF: the link closed.
            // (Skipping it instead spins forever and starves the runtime.)
            Notifications::Io(r) => r.recv().await.ok().filter(|f| !f.is_empty()),
            Notifications::Stream(s) => loop {
                // Over D-Bus an empty value is just an empty notification.
                match s.next().await {
                    Some(f) if f.is_empty() => continue,
                    other => break other,
                }
            },
        }
    }
}

// ---------- supervisor ----------

/// Own the radio for the life of the daemon: bring a session up whenever the
/// mode allows it, rebuild it after failures, and route outgoing frames to
/// whichever session is live.
pub async fn supervise(node: Arc<Node>, mut out_rx: mpsc::UnboundedReceiver<Outgoing>) {
    let current: Arc<Mutex<Option<Arc<Radio>>>> = Arc::new(Mutex::new(None));

    let dispatch = current.clone();
    tokio::spawn(async move {
        while let Some(out) = out_rx.recv().await {
            let radio = dispatch.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if let Some(radio) = radio {
                radio.send(out);
            }
        }
    });

    let session = loop {
        match Session::new().await {
            Ok(s) => break s,
            Err(e) => {
                node.set_radio(|r| {
                    r.state = RadioState::Error;
                    r.detail = Some(format!("can't reach BlueZ: {e}"));
                });
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    };

    let (eff_tx, mut eff_rx) = watch::channel(Effective::Balanced);
    tokio::spawn(power_loop(node.clone(), session.clone(), eff_tx));

    let mut failures: u32 = 0;
    loop {
        while *eff_rx.borrow_and_update() == Effective::Off {
            node.set_radio(|r| {
                r.state = RadioState::Off;
                r.detail = None;
            });
            if eff_rx.changed().await.is_err() {
                return;
            }
        }
        node.set_radio(|r| {
            r.state = RadioState::Starting;
            r.detail = None;
        });
        let started = Instant::now();
        let end = run_session(&node, &session, &current, eff_rx.clone()).await;
        // A session that ran a while was healthy; start the backoff over.
        if started.elapsed() >= STABLE_SESSION {
            failures = 0;
        }
        let retry = match end {
            SessionEnd::Stopped => Duration::ZERO,
            SessionEnd::NoAdapter => {
                node.set_radio(|r| {
                    r.state = RadioState::NoAdapter;
                    r.detail = Some("no Bluetooth adapter".into());
                });
                Duration::from_secs(10)
            }
            SessionEnd::AdapterOff => {
                node.set_radio(|r| {
                    r.state = RadioState::AdapterOff;
                    r.detail = Some("Bluetooth is off".into());
                });
                Duration::from_secs(3)
            }
            SessionEnd::Failed(e) => {
                // Back off exponentially: if bluetoothd keeps dying (it has a
                // crash we can trip), hammering it only makes a crash loop.
                failures += 1;
                let wait = RETRY_BASE.saturating_mul(1 << (failures - 1).min(6)).min(RETRY_MAX);
                tracing::warn!("radio session ended: {e:#}; retrying in {}s", wait.as_secs());
                node.set_radio(|r| {
                    r.state = RadioState::Error;
                    r.detail = Some(format!("{e:#}"));
                });
                wait
            }
        };
        if !retry.is_zero() {
            tokio::select! {
                _ = tokio::time::sleep(retry) => {}
                _ = eff_rx.changed() => {}
            }
        }
    }
}

async fn run_session(
    node: &Arc<Node>,
    session: &Session,
    current: &Arc<Mutex<Option<Arc<Radio>>>>,
    mut effective: watch::Receiver<Effective>,
) -> SessionEnd {
    let adapter = match session.default_adapter().await {
        Ok(a) => a,
        Err(_) => return SessionEnd::NoAdapter,
    };
    match adapter.is_powered().await {
        Ok(true) => {}
        Ok(false) => return SessionEnd::AdapterOff,
        Err(e) => return SessionEnd::Failed(e.into()),
    }

    let radio = Radio::new(node.clone(), adapter.clone());
    let handles: Result<(ApplicationHandle, AdvertisementHandle)> = async {
        let app = adapter
            .serve_gatt_application(radio.application())
            .await
            .context("registering GATT service")?;
        let adv = adapter.advertise(advertisement()).await.context("starting advertising")?;
        Ok((app, adv))
    }
    .await;
    let (app, adv) = match handles {
        Ok(h) => h,
        Err(e) => return SessionEnd::Failed(e),
    };

    *current.lock().unwrap_or_else(|e| e.into_inner()) = Some(radio.clone());
    node.set_radio(|r| {
        r.state = RadioState::Running;
        r.detail = None;
        r.advertising = true;
    });
    tracing::info!("advertising bitchat on {}", adapter.name());

    radio.spawn(radio.clone().scan_loop(effective.clone()));
    radio.spawn(radio.clone().monitor_loop());

    let end = loop {
        tokio::select! {
            changed = effective.changed() => {
                if changed.is_err() || *effective.borrow() == Effective::Off {
                    break SessionEnd::Stopped;
                }
            }
            _ = tokio::time::sleep(HEALTH_INTERVAL) => {
                match adapter.is_powered().await {
                    Ok(true) => {}
                    Ok(false) => break SessionEnd::AdapterOff,
                    Err(e) => break SessionEnd::Failed(anyhow!("adapter went away: {e}")),
                }
                // After a bluetoothd restart our registrations are gone even
                // though every call succeeds again; this is how we notice.
                if adapter.active_advertising_instances().await.unwrap_or(0) == 0 {
                    break SessionEnd::Failed(anyhow!("BlueZ dropped our advertisement; re-registering"));
                }
            }
        }
    };

    *current.lock().unwrap_or_else(|e| e.into_inner()) = None;
    radio.shutdown().await;
    drop(adv);
    drop(app);
    node.set_radio(|r| {
        r.advertising = false;
        r.scanning = false;
        r.links = 0;
    });
    end
}

fn advertisement() -> Advertisement {
    // Flags plus one 128-bit UUID: 21 bytes, inside a legacy advertisement
    // so every phone's scanner sees it.
    Advertisement {
        advertisement_type: AdvType::Peripheral,
        service_uuids: [SERVICE].into_iter().collect(),
        discoverable: Some(true),
        ..Default::default()
    }
}

/// Recompute the effective mode from the user's setting, AC power and
/// Bluetooth audio, on a timer and whenever the setting changes.
async fn power_loop(node: Arc<Node>, session: Session, eff_tx: watch::Sender<Effective>) {
    let mut mode_rx = node.mode();
    loop {
        let mode = *mode_rx.borrow_and_update();
        let on_battery = power::on_battery();
        let audio = if mode == Mode::Auto { audio_active(&session).await } else { false };
        let effective = power::resolve(mode, on_battery, audio);
        eff_tx.send_if_modified(|e| {
            let changed = *e != effective;
            *e = effective;
            changed
        });
        node.set_radio(|r| {
            r.effective = effective;
            r.on_battery = on_battery;
            r.audio = audio;
        });
        tokio::select! {
            _ = tokio::time::sleep(POWER_INTERVAL) => {}
            changed = mode_rx.changed() => if changed.is_err() { return },
        }
    }
}

/// Is a connected device an audio sink (headphones, speaker)?
async fn audio_active(session: &Session) -> bool {
    let Ok(adapter) = session.default_adapter().await else { return false };
    let Ok(addrs) = adapter.device_addresses().await else { return false };
    let sink: Uuid = power::AUDIO_SINK_UUID.parse().expect("valid uuid");
    for addr in addrs {
        let Ok(d) = adapter.device(addr) else { continue };
        if d.is_connected().await.unwrap_or(false)
            && d.uuids().await.ok().flatten().is_some_and(|u| u.contains(&sink))
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitchat_proto::Identity;

    #[test]
    fn frame_limits() {
        assert_eq!(frame_limit(514), 512);
        assert_eq!(frame_limit(182), 182);
        assert_eq!(frame_limit(20), 20);
    }

    #[test]
    fn frames_fit_the_link() {
        let id = Identity::generate();
        let text: String = (0..900u32).map(|i| char::from(b'a' + ((i * 7 + i / 5) % 26) as u8)).collect();
        let packet = id.message_packet(&text);
        for limit in [182, 244, 512] {
            let out = frames(&packet, limit);
            assert!(!out.is_empty());
            assert!(out.iter().all(|f| f.len() <= limit), "limit {limit}");
        }
    }

    #[test]
    fn noise_frames_drop_padding_when_it_would_overflow() {
        let mut p = Packet::new(bitchat_proto::MessageType::NoiseEncrypted, bitchat_proto::PeerId([1; 8]), vec![7; 150]);
        p.recipient = Some(bitchat_proto::PeerId([2; 8]));
        // 180 unpadded bytes would pad to 256; a 182-byte link can't take that.
        let out = frames(&p, 182);
        assert_eq!(out.len(), 1);
        assert!(out[0].len() <= 182);
        assert_eq!(frames(&p, 512)[0].len(), 256);
    }
}
