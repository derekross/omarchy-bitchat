//! PKCS#7-style block padding (`MessagePadding.kt`).

const BLOCK_SIZES: [usize; 4] = [256, 512, 1024, 2048];

/// Smallest block that fits `len` plus 16 bytes of slack, or `len` itself
/// when nothing fits (big frames get fragmented anyway).
pub fn optimal_block_size(len: usize) -> usize {
    let total = len + 16;
    BLOCK_SIZES
        .iter()
        .copied()
        .find(|&block| total <= block)
        .unwrap_or(len)
}

/// Pad to `target` with bytes equal to the pad length. A no-op when the pad
/// would be empty or would not fit the single-byte marker.
pub fn pad(mut data: Vec<u8>, target: usize) -> Vec<u8> {
    if data.len() >= target {
        return data;
    }
    let needed = target - data.len();
    if needed > 255 {
        return data;
    }
    data.resize(target, needed as u8);
    data
}

/// Strip valid PKCS#7 padding; returns the input unchanged when the tail is
/// not a valid pad.
pub fn unpad(data: &[u8]) -> &[u8] {
    let Some(&last) = data.last() else {
        return data;
    };
    let n = last as usize;
    if n == 0 || n > data.len() {
        return data;
    }
    let start = data.len() - n;
    if data[start..].iter().all(|&b| b == last) {
        &data[..start]
    } else {
        data
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_choice() {
        assert_eq!(optimal_block_size(10), 256);
        assert_eq!(optimal_block_size(240), 256);
        assert_eq!(optimal_block_size(241), 512);
        assert_eq!(optimal_block_size(2032), 2048);
        assert_eq!(optimal_block_size(2033), 2033);
    }

    #[test]
    fn pad_round_trip() {
        let data = vec![1, 2, 3];
        let padded = pad(data.clone(), 256);
        assert_eq!(padded.len(), 256);
        assert!(padded[3..].iter().all(|&b| b == 253));
        assert_eq!(unpad(&padded), &data[..]);
    }

    #[test]
    fn pad_skips_oversized_gap() {
        // 300 bytes of padding cannot be expressed in one marker byte.
        let data = vec![9; 212];
        assert_eq!(pad(data.clone(), 512), data);
    }

    #[test]
    fn unpad_rejects_invalid() {
        assert_eq!(unpad(&[1, 2, 3, 2]), &[1, 2, 3, 2]);
        assert_eq!(unpad(&[1, 2, 0]), &[1, 2, 0]);
        assert_eq!(unpad(&[]), &[] as &[u8]);
    }
}
