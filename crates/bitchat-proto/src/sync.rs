//! Gossip sync (`GCSFilter.kt`, `RequestSyncPacket.kt`, `GossipSyncManager.kt`).
//!
//! A node asks its neighbors "here's what I have" as a Golomb-coded set of
//! packet IDs; each neighbor answers with the stored packets whose IDs
//! aren't in the set. Requests use TTL 0 and never leave the link.
//!
//! REQUEST_SYNC payload, TLV with 16-bit lengths:
//! `0x01` P (u8), `0x02` M (u32 BE), `0x03` GCS bitstream, and on iOS
//! `0x04` type flags (LE bitfield), `0x05` since timestamp, `0x06` fragment IDs.

use sha2::{Digest, Sha256};

/// Receivers refuse bigger filters.
pub const MAX_FILTER_BYTES: usize = 1024;
/// What we build: Android's default.
pub const DEFAULT_FILTER_BYTES: usize = 256;
/// Target false-positive rate 1% gives P = 7.
pub const DEFAULT_P: u8 = 7;

/// Type-flag bits (iOS TLV 0x04). Absent means announce + message.
pub const TYPE_ANNOUNCE: u64 = 1 << 0;
pub const TYPE_MESSAGE: u64 = 1 << 1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestSync {
    pub p: u8,
    pub m: u32,
    pub data: Vec<u8>,
    pub types: Option<u64>,
    /// iOS TLV 0x05: the filter only covers packets at or after this time;
    /// older ones are outside it, not missing.
    pub since: Option<u64>,
}

impl RequestSync {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        put_tlv(&mut out, 0x01, &[self.p]);
        put_tlv(&mut out, 0x02, &self.m.to_be_bytes());
        put_tlv(&mut out, 0x03, &self.data);
        if let Some(ts) = self.since {
            put_tlv(&mut out, 0x05, &ts.to_be_bytes());
        }
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<RequestSync> {
        let (mut p, mut m, mut data, mut types, mut since) = (None, None, None, None, None);
        let mut i = 0;
        while i + 3 <= bytes.len() {
            let t = bytes[i];
            let len = u16::from_be_bytes([bytes[i + 1], bytes[i + 2]]) as usize;
            i += 3;
            let v = bytes.get(i..i + len)?;
            i += len;
            match t {
                0x01 if len == 1 => p = Some(v[0]),
                0x02 if len == 4 => m = Some(u32::from_be_bytes(v.try_into().ok()?)),
                0x03 => {
                    if len > MAX_FILTER_BYTES {
                        return None;
                    }
                    data = Some(v.to_vec());
                }
                0x04 => {
                    let mut raw = 0u64;
                    for (k, &b) in v.iter().take(8).enumerate() {
                        raw |= (b as u64) << (8 * k);
                    }
                    types = Some(raw);
                }
                0x05 if len == 8 => since = Some(u64::from_be_bytes(v.try_into().ok()?)),
                _ => {}
            }
        }
        let (p, m, data) = (p?, m?, data?);
        (p >= 1 && m > 0).then_some(RequestSync { p, m, data, types, since })
    }

    /// Does the requester want packets of this kind?
    pub fn wants(&self, flag: u64) -> bool {
        self.types.is_none_or(|t| t & flag != 0)
    }

    /// Decode the filter for membership tests.
    pub fn filter(&self) -> Gcs {
        Gcs { m: self.m as u64, sorted: decode_values(self.p, self.m as u64, &self.data) }
    }
}

fn put_tlv(out: &mut Vec<u8>, t: u8, v: &[u8]) {
    out.push(t);
    out.extend_from_slice(&(v.len() as u16).to_be_bytes());
    out.extend_from_slice(v);
}

/// First 8 bytes of SHA-256 over the 16-byte packet ID, as a positive i64.
pub fn h64(id: &[u8; 16]) -> u64 {
    let d = Sha256::digest(id);
    u64::from_be_bytes(d[..8].try_into().expect("8 bytes")) & 0x7fff_ffff_ffff_ffff
}

fn map(id: &[u8; 16], m: u64) -> u64 {
    match h64(id) % m {
        0 => 1,
        v => v,
    }
}

/// A decoded filter.
pub struct Gcs {
    m: u64,
    sorted: Vec<u64>,
}

impl Gcs {
    pub fn might_contain(&self, id: &[u8; 16]) -> bool {
        self.sorted.binary_search(&map(id, self.m)).is_ok()
    }
}

/// Build a request advertising `entries` (packet ID and timestamp, newest
/// first). The oldest are dropped when they don't fit `max_bytes`, and then
/// the request carries a `since` cursor so neighbors don't resend that tail.
pub fn build_request(entries: &[([u8; 16], u64)], max_bytes: usize) -> RequestSync {
    let ids: Vec<[u8; 16]> = entries.iter().map(|(id, _)| *id).collect();
    let (mut req, covered) = build_filter(&ids, max_bytes);
    if covered > 0 && covered < entries.len() {
        req.since = Some(entries[covered - 1].1);
    }
    req
}

/// The filter alone, plus how many of `ids` it covers.
fn build_filter(ids: &[[u8; 16]], max_bytes: usize) -> (RequestSync, usize) {
    let p = DEFAULT_P;
    // Expected bits per element is about P + 2.
    let cap = ((max_bytes * 8) / (p as usize + 2)).max(1);
    let mut n = ids.len().min(cap);
    if n == 0 {
        return (RequestSync { p, m: 1, data: Vec::new(), types: None, since: None }, 0);
    }
    loop {
        let m = ((n as u64) << p).clamp(1, u32::MAX as u64);
        let mut values: Vec<u64> = ids[..n].iter().map(|id| map(id, m)).collect();
        values.sort_unstable();
        values.dedup();
        let data = encode_values(&values, p);
        if data.len() <= max_bytes || n <= 1 {
            return (RequestSync { p, m: m as u32, data, types: None, since: None }, n);
        }
        n = (n * 9) / 10;
    }
}

fn encode_values(sorted: &[u64], p: u8) -> Vec<u8> {
    let mut w = BitWriter::default();
    let mask = (1u64 << p) - 1;
    let mut prev = 0;
    for &v in sorted {
        let x = v - prev;
        prev = v;
        let q = (x - 1) >> p;
        for _ in 0..q {
            w.bit(1);
        }
        w.bit(0);
        w.bits((x - 1) & mask, p);
    }
    w.finish()
}

fn decode_values(p: u8, m: u64, data: &[u8]) -> Vec<u64> {
    let mut r = BitReader { data, pos: 0 };
    let mut out = Vec::new();
    let mut acc = 0u64;
    // A hostile filter could claim huge quotients; bound the work.
    while r.pos < data.len() * 8 && out.len() < MAX_FILTER_BYTES * 8 {
        let mut q = 0u64;
        loop {
            match r.bit() {
                Some(1) => q += 1,
                Some(_) => break,
                None => return out,
            }
        }
        let Some(rem) = r.bits(p) else { return out };
        let Some(x) = q.checked_shl(p as u32).and_then(|v| v.checked_add(rem + 1)) else { return out };
        acc = acc.saturating_add(x);
        if acc >= m {
            break;
        }
        out.push(acc);
    }
    out
}

#[derive(Default)]
struct BitWriter {
    out: Vec<u8>,
    cur: u8,
    n: u8,
}

impl BitWriter {
    fn bit(&mut self, b: u8) {
        self.cur = (self.cur << 1) | (b & 1);
        self.n += 1;
        if self.n == 8 {
            self.out.push(self.cur);
            self.cur = 0;
            self.n = 0;
        }
    }
    fn bits(&mut self, v: u64, count: u8) {
        for i in (0..count).rev() {
            self.bit(((v >> i) & 1) as u8);
        }
    }
    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push(self.cur << (8 - self.n));
        }
        self.out
    }
}

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl BitReader<'_> {
    fn bit(&mut self) -> Option<u8> {
        let byte = *self.data.get(self.pos / 8)?;
        let b = (byte >> (7 - (self.pos % 8))) & 1;
        self.pos += 1;
        Some(b)
    }
    fn bits(&mut self, count: u8) -> Option<u64> {
        let mut v = 0;
        for _ in 0..count {
            v = (v << 1) | self.bit()? as u64;
        }
        Some(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(n: usize, seed: u8) -> Vec<[u8; 16]> {
        (0..n)
            .map(|i| {
                let d = Sha256::digest([seed, (i >> 8) as u8, i as u8]);
                d[..16].try_into().unwrap()
            })
            .collect()
    }

    /// Newest first: timestamps count down.
    fn entries(n: usize, seed: u8) -> Vec<([u8; 16], u64)> {
        ids(n, seed).into_iter().enumerate().map(|(i, id)| (id, 1_000_000 - i as u64)).collect()
    }

    #[test]
    fn members_are_found() {
        let have = ids(150, 1);
        let req = build_request(&entries(150, 1), DEFAULT_FILTER_BYTES);
        assert!(req.data.len() <= DEFAULT_FILTER_BYTES);
        assert_eq!(req.since, None, "everything fits, so no cursor");
        let f = RequestSync::decode(&req.encode()).unwrap().filter();
        for id in &have {
            assert!(f.might_contain(id));
        }
    }

    #[test]
    fn false_positive_rate_is_low() {
        let f = build_request(&entries(150, 1), DEFAULT_FILTER_BYTES).filter();
        let fp = ids(2000, 2).iter().filter(|id| f.might_contain(id)).count();
        assert!(fp < 60, "{fp} false positives of 2000");
    }

    #[test]
    fn oversized_sets_are_trimmed_to_fit() {
        let have = entries(2000, 3);
        let req = build_request(&have, DEFAULT_FILTER_BYTES);
        assert!(req.data.len() <= DEFAULT_FILTER_BYTES);
        // The newest (first) IDs are the ones kept, and the cursor says so.
        let back = RequestSync::decode(&req.encode()).unwrap();
        let f = back.filter();
        assert!(have[..50].iter().all(|(id, _)| f.might_contain(id)));
        let since = back.since.unwrap();
        assert!(since < have[0].1 && since > have[1999].1);
    }

    #[test]
    fn empty_request() {
        let req = build_request(&[], DEFAULT_FILTER_BYTES);
        assert_eq!(req.since, None);
        let back = RequestSync::decode(&req.encode()).unwrap();
        assert_eq!(back.m, 1);
        assert!(!back.filter().might_contain(&[0; 16]));
    }

    #[test]
    fn tlv_layout_and_limits() {
        let req = RequestSync { p: 7, m: 0x01020304, data: vec![0xAB], types: None, since: None };
        assert_eq!(req.encode(), vec![0x01, 0, 1, 7, 0x02, 0, 4, 1, 2, 3, 4, 0x03, 0, 1, 0xAB]);
        let mut big = vec![0x01, 0, 1, 7, 0x02, 0, 4, 0, 0, 1, 0, 0x03, 0x04, 0x01];
        big.extend(vec![0; 1025]);
        assert!(RequestSync::decode(&big).is_none());
        assert!(RequestSync::decode(&[0x01, 0, 1, 0, 0x02, 0, 4, 0, 0, 0, 1, 0x03, 0, 0]).is_none());
    }

    #[test]
    fn ios_type_flags() {
        let mut bytes = RequestSync { p: 7, m: 1, data: vec![], types: None, since: None }.encode();
        assert!(RequestSync::decode(&bytes).unwrap().wants(TYPE_MESSAGE));
        bytes.extend([0x04, 0, 1, TYPE_ANNOUNCE as u8]);
        let req = RequestSync::decode(&bytes).unwrap();
        assert!(req.wants(TYPE_ANNOUNCE));
        assert!(!req.wants(TYPE_MESSAGE));
    }

    #[test]
    fn since_tlv_is_big_endian() {
        let req = RequestSync { p: 7, m: 1, data: vec![], types: None, since: Some(0x0102030405060708) };
        let bytes = req.encode();
        assert_eq!(&bytes[bytes.len() - 11..], &[0x05, 0, 8, 1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(RequestSync::decode(&bytes).unwrap().since, req.since);
    }

    #[test]
    fn hostile_filter_is_bounded() {
        // All ones: an endless unary quotient.
        let req = RequestSync { p: 7, m: u32::MAX, data: vec![0xFF; 1024], types: None, since: None };
        assert!(req.filter().sorted.is_empty());
    }
}
