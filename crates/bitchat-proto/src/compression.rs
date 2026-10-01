//! Raw DEFLATE payload compression (`CompressionUtil.kt`).
//!
//! iOS's `COMPRESSION_ZLIB` emits raw deflate with no zlib header, and Android
//! matches it. Decoding also accepts a zlib-wrapped stream, as Android does.

use std::io::{Read, Write};

use flate2::Compression;
use flate2::read::{DeflateDecoder, ZlibDecoder};
use flate2::write::DeflateEncoder;

pub const COMPRESSION_THRESHOLD: usize = 100;
pub const MAX_RATIO: f64 = 50_000.0;

/// Same heuristic as both apps: big enough, and not already high-entropy.
pub fn should_compress(data: &[u8]) -> bool {
    if data.len() < COMPRESSION_THRESHOLD {
        return false;
    }
    let mut seen = [false; 256];
    let mut unique = 0usize;
    for &b in data {
        if !seen[b as usize] {
            seen[b as usize] = true;
            unique += 1;
        }
    }
    (unique as f64) / (data.len().min(256) as f64) < 0.9
}

/// Compress, returning `None` unless the result is smaller than the input.
pub fn compress(data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < COMPRESSION_THRESHOLD {
        return None;
    }
    // Level 6 is java.util.zip.Deflater.DEFAULT_COMPRESSION.
    let mut enc = DeflateEncoder::new(Vec::with_capacity(data.len()), Compression::new(6));
    enc.write_all(data).ok()?;
    let out = enc.finish().ok()?;
    (!out.is_empty() && out.len() < data.len()).then_some(out)
}

/// Inflate to exactly `original_size` bytes: under- or over-declared streams
/// and trailing garbage are rejected.
pub fn decompress(data: &[u8], original_size: usize) -> Option<Vec<u8>> {
    if data.is_empty() || original_size == 0 || original_size > crate::MAX_PAYLOAD_LENGTH {
        return None;
    }
    if looks_like_zlib(data) {
        let mut z = ZlibDecoder::new(data);
        if let Some(out) = inflate_exact(&mut z, original_size)
            && z.total_in() as usize == data.len()
        {
            return Some(out);
        }
    }
    let mut d = DeflateDecoder::new(data);
    let out = inflate_exact(&mut d, original_size)?;
    // Trailing bytes after the stream are rejected, as on Android.
    (d.total_in() as usize == data.len()).then_some(out)
}

fn inflate_exact<R: Read>(reader: &mut R, original_size: usize) -> Option<Vec<u8>> {
    // Grow as data arrives rather than trusting the declared size up front.
    let mut out = Vec::with_capacity(original_size.min(64 * 1024));
    // Read one byte past the declared size to catch an under-declared stream.
    reader
        .take(original_size as u64 + 1)
        .read_to_end(&mut out)
        .ok()?;
    (out.len() == original_size).then_some(out)
}

fn looks_like_zlib(data: &[u8]) -> bool {
    if data.len() < 2 {
        return false;
    }
    let cmf = data[0] as u16;
    let flg = data[1] as u16;
    (cmf & 0x0F) == 8 && (cmf >> 4) <= 7 && ((cmf << 8) | flg).is_multiple_of(31)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heuristic() {
        assert!(!should_compress(&[b'a'; 99]));
        assert!(should_compress(&[b'a'; 100]));
        let high_entropy: Vec<u8> = (0..=255u8).collect();
        assert!(!should_compress(&high_entropy));
    }

    #[test]
    fn round_trip() {
        let text = "This is a test message that should compress well. ".repeat(10);
        let packed = compress(text.as_bytes()).unwrap();
        assert!(packed.len() < text.len());
        assert_eq!(decompress(&packed, text.len()).unwrap(), text.as_bytes());
    }

    #[test]
    fn rejects_wrong_declared_size() {
        let text = "abcabcabc".repeat(40);
        let packed = compress(text.as_bytes()).unwrap();
        assert!(decompress(&packed, text.len() - 1).is_none());
        assert!(decompress(&packed, text.len() + 1).is_none());
    }

    #[test]
    fn rejects_trailing_bytes() {
        let text = "trailing ".repeat(30);
        let mut packed = compress(text.as_bytes()).unwrap();
        packed.extend_from_slice(b"junk");
        assert!(decompress(&packed, text.len()).is_none());
    }

    #[test]
    fn accepts_zlib_wrapped() {
        let text = "zlib wrapped legacy payload ".repeat(20);
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(text.as_bytes()).unwrap();
        let packed = enc.finish().unwrap();
        assert_eq!(decompress(&packed, text.len()).unwrap(), text.as_bytes());
    }
}
