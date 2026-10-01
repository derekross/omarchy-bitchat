//! The bitchat BLE mesh wire protocol.
//!
//! Byte-compatible with bitchat-android (`protocol/BinaryProtocol.kt` and
//! friends) and bitchat iOS (`BitFoundation/BinaryProtocol.swift`). Pure: no
//! I/O, no clocks except where a caller passes one in.

pub mod announce;
pub mod compression;
pub mod dedup;
pub mod fragment;
pub mod identity;
pub mod packet;
pub mod padding;
pub mod peer_id;
pub mod relay;
pub mod sync;

pub use announce::{Announcement, Capabilities};
pub use identity::Identity;
pub use packet::{MessageType, Packet};
pub use peer_id::PeerId;

/// GATT service every bitchat node hosts and scans for (release builds;
/// iOS debug builds use `...4B5A` and will not see us).
pub const SERVICE_UUID: u128 = 0xF47B5E2D_4A9E_4C5A_9B3F_8E1D2C3A4B5C;
/// The single characteristic carrying frames in both directions.
pub const CHARACTERISTIC_UUID: u128 = 0xA1B2C3D4_E5F6_4A5B_8C9D_0E1F2A3B4C5D;

/// Default hop count for packets we originate.
pub const MAX_TTL: u8 = 7;
/// Frames above this many bytes are fragmented (Android and iOS agree).
pub const FRAGMENT_THRESHOLD: usize = 512;
/// Largest payload a receiver accepts (Android `MAX_PAYLOAD_LENGTH`).
pub const MAX_PAYLOAD_LENGTH: usize = 10 * 1024 * 1024;

/// Milliseconds since the Unix epoch, as packets carry it.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
