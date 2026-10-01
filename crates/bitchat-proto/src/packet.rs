//! The binary packet (`BinaryProtocol.kt`, `BinaryProtocol.swift`).
//!
//! ```text
//! version u8 | type u8 | ttl u8 | timestamp u64 | flags u8 | payloadLength u16 (v1) / u32 (v2)
//! senderID [8] | recipientID [8]? | route (v2: count u8 + N*[8])? |
//! originalSize u16/u32 (if compressed) + payload | signature [64]?
//! ```
//! All integers big-endian. `payloadLength` covers the original-size field
//! and the payload, not the route.

use thiserror::Error;

use crate::compression;
use crate::padding;
use crate::peer_id::PeerId;

pub mod flags {
    pub const HAS_RECIPIENT: u8 = 0x01;
    pub const HAS_SIGNATURE: u8 = 0x02;
    pub const IS_COMPRESSED: u8 = 0x04;
    /// Only meaningful on v2 packets.
    pub const HAS_ROUTE: u8 = 0x08;
    /// Request-sync response (iOS). Android ignores it.
    pub const IS_RSR: u8 = 0x10;
}

const HEADER_V1: usize = 14;
const HEADER_V2: usize = 16;
const ID_LEN: usize = 8;
pub const SIGNATURE_LEN: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum MessageType {
    Announce = 0x01,
    Message = 0x02,
    Leave = 0x03,
    CourierEnvelope = 0x04,
    NoiseHandshake = 0x10,
    NoiseEncrypted = 0x11,
    Fragment = 0x20,
    RequestSync = 0x21,
    FileTransfer = 0x22,
    BoardPost = 0x23,
    PrekeyBundle = 0x24,
    GroupMessage = 0x25,
    Ping = 0x26,
    Pong = 0x27,
    NostrCarrier = 0x28,
    VoiceFrame = 0x29,
}

impl MessageType {
    pub fn from_u8(v: u8) -> Option<MessageType> {
        use MessageType::*;
        Some(match v {
            0x01 => Announce,
            0x02 => Message,
            0x03 => Leave,
            0x04 => CourierEnvelope,
            0x10 => NoiseHandshake,
            0x11 => NoiseEncrypted,
            0x20 => Fragment,
            0x21 => RequestSync,
            0x22 => FileTransfer,
            0x23 => BoardPost,
            0x24 => PrekeyBundle,
            0x25 => GroupMessage,
            0x26 => Ping,
            0x27 => Pong,
            0x28 => NostrCarrier,
            0x29 => VoiceFrame,
            _ => return None,
        })
    }

    /// Only Noise frames are padded on BLE (`BLEPacketPaddingPolicy.kt`).
    pub fn pads_on_ble(v: u8) -> bool {
        v == MessageType::NoiseHandshake as u8 || v == MessageType::NoiseEncrypted as u8
    }
}

/// The payload bytes as received, so a re-encode (signature check, relay)
/// reproduces the originator's compression exactly. DEFLATE output differs
/// between encoders, so recompressing could break a valid signature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WirePayload {
    pub bytes: Vec<u8>,
    pub compressed: bool,
    /// The decoded payload these bytes stand for; replacing the payload
    /// invalidates them.
    pub for_payload: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packet {
    pub version: u8,
    pub ptype: u8,
    pub ttl: u8,
    pub timestamp: u64,
    pub sender: PeerId,
    pub recipient: Option<PeerId>,
    /// Source route: intermediate hops only. Encoded on v2 packets only.
    pub route: Vec<PeerId>,
    pub payload: Vec<u8>,
    pub signature: Option<[u8; SIGNATURE_LEN]>,
    pub rsr: bool,
    pub wire: Option<WirePayload>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum EncodeError {
    #[error("payload of {0} bytes exceeds the receiver limit")]
    PayloadTooLarge(usize),
    #[error("payload of {0} bytes does not fit a v1 length field")]
    V1Overflow(usize),
}

impl Packet {
    /// A v1 packet with the default TTL, stamped now.
    pub fn new(ptype: MessageType, sender: PeerId, payload: Vec<u8>) -> Packet {
        Packet {
            version: 1,
            ptype: ptype as u8,
            ttl: crate::MAX_TTL,
            timestamp: crate::now_ms(),
            sender,
            recipient: None,
            route: Vec::new(),
            payload,
            signature: None,
            rsr: false,
            wire: None,
        }
    }

    pub fn message_type(&self) -> Option<MessageType> {
        MessageType::from_u8(self.ptype)
    }

    /// Broadcast means no recipient or the all-FF recipient.
    pub fn is_broadcast(&self) -> bool {
        self.recipient.is_none_or(|r| r.is_broadcast())
    }

    pub fn encode(&self, pad: bool) -> Result<Vec<u8>, EncodeError> {
        if self.payload.len() > crate::MAX_PAYLOAD_LENGTH {
            return Err(EncodeError::PayloadTooLarge(self.payload.len()));
        }
        let v2 = self.version >= 2;

        let mut body: &[u8] = &self.payload;
        let compressed_buf;
        let mut compressed = false;
        match &self.wire {
            Some(wire) if wire.for_payload == self.payload => {
                if wire.compressed {
                    body = &wire.bytes;
                    compressed = true;
                }
                // Uncompressed on the wire stays uncompressed.
            }
            _ => {
                if compression::should_compress(&self.payload)
                    && let Some(c) = compression::compress(&self.payload) {
                        compressed_buf = c;
                        body = &compressed_buf;
                        compressed = true;
                    }
            }
        }

        let size_field = if compressed { if v2 { 4 } else { 2 } } else { 0 };
        let payload_len = body.len() + size_field;
        if !v2 && (payload_len > 0xFFFF || (compressed && self.payload.len() > 0xFFFF)) {
            return Err(EncodeError::V1Overflow(payload_len));
        }
        let route: &[PeerId] = if v2 { &self.route[..self.route.len().min(255)] } else { &[] };

        let mut flags = 0u8;
        if self.recipient.is_some() {
            flags |= flags::HAS_RECIPIENT;
        }
        if self.signature.is_some() {
            flags |= flags::HAS_SIGNATURE;
        }
        if compressed {
            flags |= flags::IS_COMPRESSED;
        }
        if !route.is_empty() {
            flags |= flags::HAS_ROUTE;
        }
        if self.rsr {
            flags |= flags::IS_RSR;
        }

        let mut out = Vec::with_capacity(
            HEADER_V2 + 2 * ID_LEN + 1 + route.len() * ID_LEN + payload_len + SIGNATURE_LEN,
        );
        out.push(self.version);
        out.push(self.ptype);
        out.push(self.ttl);
        out.extend_from_slice(&self.timestamp.to_be_bytes());
        out.push(flags);
        if v2 {
            out.extend_from_slice(&(payload_len as u32).to_be_bytes());
        } else {
            out.extend_from_slice(&(payload_len as u16).to_be_bytes());
        }
        out.extend_from_slice(&self.sender.0);
        if let Some(r) = self.recipient {
            out.extend_from_slice(&r.0);
        }
        if !route.is_empty() {
            out.push(route.len() as u8);
            for hop in route {
                out.extend_from_slice(&hop.0);
            }
        }
        if compressed {
            if v2 {
                out.extend_from_slice(&(self.payload.len() as u32).to_be_bytes());
            } else {
                out.extend_from_slice(&(self.payload.len() as u16).to_be_bytes());
            }
        }
        out.extend_from_slice(body);
        if let Some(sig) = &self.signature {
            out.extend_from_slice(sig);
        }

        if pad {
            let target = padding::optimal_block_size(out.len());
            out = padding::pad(out, target);
        }
        Ok(out)
    }

    /// Encode for BLE: padded only for Noise types.
    pub fn encode_for_ble(&self) -> Result<Vec<u8>, EncodeError> {
        self.encode(MessageType::pads_on_ble(self.ptype))
    }

    /// The bytes an Ed25519 signature covers: the packet with TTL 0, no
    /// signature and no RSR flag, padded. TTL is excluded because relays
    /// decrement it.
    pub fn signing_preimage(&self) -> Result<Vec<u8>, EncodeError> {
        let mut unsigned = self.clone();
        unsigned.ttl = 0;
        unsigned.signature = None;
        unsigned.rsr = false;
        unsigned.encode(true)
    }

    /// The size of the unpadded frame starting at `data`, read from the
    /// header alone (no decompression). `None` until enough header bytes
    /// are present to tell. Lets a transport see whether a write holds a
    /// whole frame without decoding it.
    pub fn frame_len(data: &[u8]) -> Option<usize> {
        let version = *data.first()?;
        if version != 1 && version != 2 {
            return None;
        }
        let v2 = version == 2;
        let flag_bits = *data.get(11)?;
        let (payload_len, header) = if v2 {
            (u32::from_be_bytes(data.get(12..16)?.try_into().ok()?) as usize, HEADER_V2)
        } else {
            (u16::from_be_bytes(data.get(12..14)?.try_into().ok()?) as usize, HEADER_V1)
        };
        let mut len = header + ID_LEN + payload_len;
        if flag_bits & flags::HAS_RECIPIENT != 0 {
            len += ID_LEN;
        }
        if v2 && flag_bits & flags::HAS_ROUTE != 0 {
            let count_at = header + ID_LEN + if flag_bits & flags::HAS_RECIPIENT != 0 { ID_LEN } else { 0 };
            len += 1 + *data.get(count_at)? as usize * ID_LEN;
        }
        if flag_bits & flags::HAS_SIGNATURE != 0 {
            len += SIGNATURE_LEN;
        }
        Some(len)
    }

    /// Decode as-is first (frames are usually unpadded), then retry with
    /// PKCS#7 padding stripped.
    pub fn decode(data: &[u8]) -> Option<Packet> {
        if let Some(p) = decode_core(data) {
            return Some(p);
        }
        let unpadded = padding::unpad(data);
        if unpadded.len() == data.len() {
            return None;
        }
        decode_core(unpadded)
    }
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let slice = self.data.get(self.pos..end)?;
        self.pos = end;
        Some(slice)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_be_bytes(self.take(2)?.try_into().ok()?))
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_be_bytes(self.take(8)?.try_into().ok()?))
    }
    fn id(&mut self) -> Option<PeerId> {
        PeerId::from_slice(self.take(ID_LEN)?)
    }
}

fn decode_core(raw: &[u8]) -> Option<Packet> {
    if raw.len() < HEADER_V1 + ID_LEN {
        return None;
    }
    let mut r = Reader { data: raw, pos: 0 };
    let version = r.u8()?;
    if version != 1 && version != 2 {
        return None;
    }
    let v2 = version == 2;
    let ptype = r.u8()?;
    let ttl = r.u8()?;
    let timestamp = r.u64()?;
    let flag_bits = r.u8()?;
    let has_recipient = flag_bits & flags::HAS_RECIPIENT != 0;
    let has_signature = flag_bits & flags::HAS_SIGNATURE != 0;
    let is_compressed = flag_bits & flags::IS_COMPRESSED != 0;
    let has_route = v2 && flag_bits & flags::HAS_ROUTE != 0;
    let rsr = flag_bits & flags::IS_RSR != 0;
    let payload_len = if v2 { r.u32()? as usize } else { r.u16()? as usize };
    if payload_len > crate::MAX_PAYLOAD_LENGTH {
        return None;
    }

    let sender = r.id()?;
    let recipient = if has_recipient { Some(r.id()?) } else { None };
    let mut route = Vec::new();
    if has_route {
        let count = r.u8()? as usize;
        for _ in 0..count {
            route.push(r.id()?);
        }
    }

    let (payload, wire) = if is_compressed {
        let size_field = if v2 { 4 } else { 2 };
        if payload_len <= size_field {
            return None;
        }
        let original = if v2 { r.u32()? as usize } else { r.u16()? as usize };
        if original == 0 || original > crate::MAX_PAYLOAD_LENGTH {
            return None;
        }
        let body = r.take(payload_len - size_field)?;
        if original as f64 / body.len() as f64 > compression::MAX_RATIO {
            return None;
        }
        let payload = compression::decompress(body, original)?;
        let wire = WirePayload { bytes: body.to_vec(), compressed: true, for_payload: payload.clone() };
        (payload, wire)
    } else {
        let payload = r.take(payload_len)?.to_vec();
        let wire = WirePayload { bytes: Vec::new(), compressed: false, for_payload: payload.clone() };
        (payload, wire)
    };

    let signature = if has_signature {
        let mut sig = [0u8; SIGNATURE_LEN];
        sig.copy_from_slice(r.take(SIGNATURE_LEN)?);
        Some(sig)
    } else {
        None
    };

    // Trailing bytes are tolerated, as on Android and iOS.
    Some(Packet {
        version,
        ptype,
        ttl,
        timestamp,
        sender,
        recipient,
        route,
        payload,
        signature,
        rsr,
        wire: Some(wire),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sender() -> PeerId {
        PeerId::from_hex("aabbccddeeff0011").unwrap()
    }

    fn base() -> Packet {
        Packet {
            version: 1,
            ptype: MessageType::Message as u8,
            ttl: 7,
            timestamp: 0x0102030405060708,
            sender: sender(),
            recipient: None,
            route: Vec::new(),
            payload: b"hi".to_vec(),
            signature: None,
            rsr: false,
            wire: None,
        }
    }

    #[test]
    fn v1_layout_is_byte_exact() {
        let bytes = base().encode(false).unwrap();
        let mut expected = vec![1, 0x02, 7, 1, 2, 3, 4, 5, 6, 7, 8, 0x00, 0x00, 0x02];
        expected.extend_from_slice(&[0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x00, 0x11]);
        expected.extend_from_slice(b"hi");
        assert_eq!(bytes, expected);
    }

    #[test]
    fn recipient_and_signature_layout() {
        let mut p = base();
        p.recipient = Some(PeerId::BROADCAST);
        p.signature = Some([0x5a; 64]);
        let bytes = p.encode(false).unwrap();
        assert_eq!(bytes[11], flags::HAS_RECIPIENT | flags::HAS_SIGNATURE);
        assert_eq!(&bytes[22..30], &[0xFF; 8]);
        assert_eq!(&bytes[30..32], b"hi");
        assert_eq!(&bytes[32..], &[0x5a; 64]);
        assert_eq!(Packet::decode(&bytes).unwrap().signature, Some([0x5a; 64]));
    }

    #[test]
    fn v2_route_layout() {
        let mut p = base();
        p.version = 2;
        p.recipient = Some(PeerId::from_hex("1100ffeeddccbbaa").unwrap());
        p.route = vec![PeerId::from_hex("1234567890abcdef").unwrap()];
        let bytes = p.encode(false).unwrap();
        assert_eq!(bytes[11], flags::HAS_RECIPIENT | flags::HAS_ROUTE);
        // 4-byte length excludes the route.
        assert_eq!(&bytes[12..16], &[0, 0, 0, 2]);
        assert_eq!(bytes[32], 1);
        assert_eq!(&bytes[33..41], &PeerId::from_hex("1234567890abcdef").unwrap().0);
        let back = Packet::decode(&bytes).unwrap();
        assert_eq!(back.route, p.route);
        assert_eq!(back.payload, b"hi");
    }

    #[test]
    fn v1_ignores_route_flag_and_route() {
        let mut p = base();
        p.route = vec![PeerId([1; 8])];
        let bytes = p.encode(false).unwrap();
        assert_eq!(bytes[11] & flags::HAS_ROUTE, 0);
        assert!(Packet::decode(&bytes).unwrap().route.is_empty());
    }

    #[test]
    fn compressed_round_trip_keeps_wire_bytes() {
        let mut p = base();
        p.payload = "hello mesh ".repeat(30).into_bytes();
        let bytes = p.encode(false).unwrap();
        assert_ne!(bytes[11] & flags::IS_COMPRESSED, 0);
        let back = Packet::decode(&bytes).unwrap();
        assert_eq!(back.payload, p.payload);
        let wire = back.wire.as_ref().unwrap();
        assert!(wire.compressed);
        // Re-encoding reproduces the exact frame.
        assert_eq!(back.encode(false).unwrap(), bytes);
    }

    #[test]
    fn uncompressed_on_wire_stays_uncompressed() {
        // Build a frame whose payload would compress, but was sent raw.
        let payload = "aaaa".repeat(40).into_bytes();
        let mut p = base();
        p.payload = payload.clone();
        p.wire = Some(WirePayload { bytes: Vec::new(), compressed: false, for_payload: payload });
        let bytes = p.encode(false).unwrap();
        assert_eq!(bytes[11] & flags::IS_COMPRESSED, 0);
        let back = Packet::decode(&bytes).unwrap();
        assert_eq!(back.encode(false).unwrap(), bytes);
    }

    #[test]
    fn frame_len_from_header() {
        let mut p = base();
        p.recipient = Some(PeerId::BROADCAST);
        p.signature = Some([1; 64]);
        let bytes = p.encode(false).unwrap();
        assert_eq!(Packet::frame_len(&bytes), Some(bytes.len()));
        assert_eq!(Packet::frame_len(&bytes[..13]), None);
        assert_eq!(Packet::frame_len(&bytes[..20]), Some(bytes.len()));
        p.version = 2;
        p.route = vec![PeerId([3; 8]), PeerId([4; 8])];
        let bytes = p.encode(true).unwrap();
        assert_eq!(Packet::frame_len(&bytes), Some(padding::unpad(&bytes).len()));
    }

    #[test]
    fn padded_frames_decode() {
        let padded = base().encode(true).unwrap();
        assert_eq!(padded.len(), 256);
        assert_eq!(Packet::decode(&padded).unwrap().payload, b"hi");
    }

    #[test]
    fn rejects_bad_version_and_truncation() {
        let mut bytes = base().encode(false).unwrap();
        assert!(Packet::decode(&bytes[..bytes.len() - 1]).is_none());
        bytes[0] = 3;
        assert!(Packet::decode(&bytes).is_none());
    }

    #[test]
    fn rsr_flag_round_trips_and_is_excluded_from_preimage() {
        let mut p = base();
        p.rsr = true;
        let bytes = p.encode(false).unwrap();
        assert_ne!(bytes[11] & flags::IS_RSR, 0);
        assert!(Packet::decode(&bytes).unwrap().rsr);
        let mut plain = p.clone();
        plain.rsr = false;
        assert_eq!(p.signing_preimage().unwrap(), plain.signing_preimage().unwrap());
    }

    #[test]
    fn preimage_ignores_ttl_and_signature_and_is_padded() {
        let mut a = base();
        let mut b = base();
        a.ttl = 7;
        b.ttl = 3;
        b.signature = Some([1; 64]);
        let pa = a.signing_preimage().unwrap();
        assert_eq!(pa, b.signing_preimage().unwrap());
        assert_eq!(pa.len(), 256);
        assert_eq!(pa[2], 0);
    }

    #[test]
    fn v1_overflow_is_an_error() {
        let mut p = base();
        // High-entropy so it will not compress below the limit.
        p.payload = (0..70_000u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
        assert!(matches!(p.encode(false), Err(EncodeError::V1Overflow(_))));
        p.version = 2;
        let bytes = p.encode(false).unwrap();
        assert_eq!(Packet::decode(&bytes).unwrap().payload, p.payload);
    }
}
