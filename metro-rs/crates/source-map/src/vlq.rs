//! Base64 variable-length quantities, as used by the `mappings` field of a
//! source map. Port of `encode.js` and the decoding half of
//! `Consumer/MappingsConsumer.js`.

const VLQ_BASE_SHIFT: u32 = 5;
const VLQ_BASE_MASK: u32 = (1 << VLQ_BASE_SHIFT) - 1;
const VLQ_CONTINUATION_BIT: u32 = 1 << VLQ_BASE_SHIFT;

const CHARS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Appends the VLQ encoding of `value`.
///
/// The JavaScript version does its arithmetic with 32-bit bit operations, so
/// this wraps in the same way for values beyond 30 bits.
pub fn encode(value: i32, out: &mut Vec<u8>) {
    let mut vlq: u32 = if value < 0 {
        (value.wrapping_neg() as u32)
            .wrapping_shl(1)
            .wrapping_add(1)
    } else {
        (value as u32).wrapping_shl(1)
    };
    loop {
        let mut digit = vlq & VLQ_BASE_MASK;
        vlq >>= VLQ_BASE_SHIFT;
        if vlq > 0 {
            digit |= VLQ_CONTINUATION_BIT;
        }
        out.push(CHARS[digit as usize]);
        if vlq == 0 {
            break;
        }
    }
}

/// The value of a base64 digit, or `None` for any other byte.
#[inline]
pub fn digit_value(byte: u8) -> Option<u32> {
    match byte {
        b'A'..=b'Z' => Some((byte - b'A') as u32),
        b'a'..=b'z' => Some((byte - b'a') as u32 + 26),
        b'0'..=b'9' => Some((byte - b'0') as u32 + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

pub const SEGMENT_SEPARATOR: u8 = b',';
pub const LINE_SEPARATOR: u8 = b';';

/// A VLQ value that does not fit in 32 bits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TooLong;

/// Accumulates the base64 digits of one VLQ value.
#[derive(Default)]
pub struct VlqReader {
    value: u32,
    shift: u32,
}

/// The largest shift at which a digit still fits in 32 bits. Values that are
/// longer than this are not valid positions, and JavaScript's behaviour for
/// them depends on how its bit operations wrap, so decoders report an error
/// and let the caller defer to the JavaScript implementation.
const MAX_SHIFT: u32 = 28;

impl VlqReader {
    /// True when no digit of a value is pending.
    pub fn is_idle(&self) -> bool {
        self.shift == 0
    }

    /// Drops a partially read value.
    pub fn reset(&mut self) {
        self.value = 0;
        self.shift = 0;
    }

    /// Feeds one digit. Returns `Ok(Some(n))` when the digit completes a value,
    /// `Ok(None)` when more digits are needed, and `Err(TooLong)` for a value
    /// that does not fit in 32 bits.
    #[inline]
    pub fn push(&mut self, digit: u32) -> Result<Option<i32>, TooLong> {
        if self.shift > MAX_SHIFT {
            return Err(TooLong);
        }
        self.value += (digit & VLQ_BASE_MASK) << self.shift;
        if digit & VLQ_CONTINUATION_BIT != 0 {
            self.shift += VLQ_BASE_SHIFT;
            return Ok(None);
        }
        let negate = self.value & 1 != 0;
        let magnitude = (self.value >> 1) as i32;
        self.value = 0;
        self.shift = 0;
        Ok(Some(if negate { -magnitude } else { magnitude }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(value: i32) -> String {
        let mut out = Vec::new();
        encode(value, &mut out);
        String::from_utf8(out).unwrap()
    }

    fn dec(text: &str) -> Vec<i32> {
        let mut reader = VlqReader::default();
        let mut values = Vec::new();
        for byte in text.bytes() {
            if let Some(v) = reader.push(digit_value(byte).unwrap()).unwrap() {
                values.push(v);
            }
        }
        values
    }

    #[test]
    fn encodes_known_values() {
        assert_eq!(enc(0), "A");
        assert_eq!(enc(1), "C");
        assert_eq!(enc(-1), "D");
        assert_eq!(enc(15), "e");
        assert_eq!(enc(16), "gB");
        assert_eq!(enc(-16), "hB");
        assert_eq!(enc(123456), "gkxH");
    }

    #[test]
    fn round_trips() {
        for v in [
            0,
            1,
            -1,
            15,
            16,
            -16,
            31,
            32,
            1000,
            -1000,
            1 << 20,
            -(1 << 20),
            (1 << 29) - 1,
        ] {
            assert_eq!(dec(&enc(v)), vec![v], "value {v}");
        }
    }

    #[test]
    fn decodes_several_values_in_a_row() {
        assert_eq!(dec("AAgBC"), vec![0, 0, 16, 1]);
    }

    #[test]
    fn rejects_non_digits() {
        assert_eq!(digit_value(b','), None);
        assert_eq!(digit_value(b';'), None);
        assert_eq!(digit_value(b'-'), None);
        assert_eq!(digit_value(0xc3), None);
    }

    #[test]
    fn rejects_values_longer_than_32_bits() {
        let mut reader = VlqReader::default();
        let mut result = Ok(None);
        for _ in 0..8 {
            result = reader.push(0b100000);
        }
        assert!(result.is_err());
    }

    #[test]
    fn idle_tracks_partial_values() {
        let mut reader = VlqReader::default();
        assert!(reader.is_idle());
        reader.push(0b100001).unwrap();
        assert!(!reader.is_idle());
        reader.reset();
        assert!(reader.is_idle());
    }
}
