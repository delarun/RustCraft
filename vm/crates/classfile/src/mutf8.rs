//! Java "modified UTF-8" (JVMS §4.4.7): NUL is encoded as C0 80 and
//! supplementary characters as two 3-byte surrogates (CESU-8).

/// Decodes to UTF-16 code units, exactly preserving lone surrogates.
pub fn decode_utf16(bytes: &[u8]) -> Result<Vec<u16>, Mutf8Error> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        let (unit, len) = if b & 0x80 == 0 {
            if b == 0 {
                return Err(Mutf8Error(i));
            }
            (b as u16, 1)
        } else if b & 0xE0 == 0xC0 {
            let b2 = continuation(bytes, i + 1)?;
            ((((b & 0x1F) as u16) << 6) | b2, 2)
        } else if b & 0xF0 == 0xE0 {
            let b2 = continuation(bytes, i + 1)?;
            let b3 = continuation(bytes, i + 2)?;
            ((((b & 0x0F) as u16) << 12) | (b2 << 6) | b3, 3)
        } else {
            return Err(Mutf8Error(i));
        };
        out.push(unit);
        i += len;
    }
    Ok(out)
}

fn continuation(bytes: &[u8], i: usize) -> Result<u16, Mutf8Error> {
    match bytes.get(i) {
        Some(&b) if b & 0xC0 == 0x80 => Ok((b & 0x3F) as u16),
        _ => Err(Mutf8Error(i)),
    }
}

/// Decodes to a Rust string; lone surrogates become U+FFFD. Fine for names and
/// descriptors, which never contain them in practice.
pub fn decode(bytes: &[u8]) -> Result<String, Mutf8Error> {
    if bytes.is_ascii() && !bytes.contains(&0) {
        // SAFETY: ASCII is valid UTF-8.
        return Ok(unsafe { String::from_utf8_unchecked(bytes.to_vec()) });
    }
    Ok(String::from_utf16_lossy(&decode_utf16(bytes)?))
}

pub fn encode(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    for unit in s.encode_utf16() {
        encode_unit(unit, &mut out);
    }
    out
}

pub fn encode_utf16(units: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(units.len());
    for &unit in units {
        encode_unit(unit, &mut out);
    }
    out
}

fn encode_unit(unit: u16, out: &mut Vec<u8>) {
    match unit {
        0x0001..=0x007F => out.push(unit as u8),
        0x0000 | 0x0080..=0x07FF => {
            out.push(0xC0 | (unit >> 6) as u8);
            out.push(0x80 | (unit & 0x3F) as u8);
        }
        _ => {
            out.push(0xE0 | (unit >> 12) as u8);
            out.push(0x80 | ((unit >> 6) & 0x3F) as u8);
            out.push(0x80 | (unit & 0x3F) as u8);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mutf8Error(pub usize);

impl std::fmt::Display for Mutf8Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "malformed modified UTF-8 at byte {}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_special_characters() {
        for s in ["", "hello", "nul\0inside", "кириллица", "😀 emoji", "\u{7FF}\u{800}\u{FFFF}"] {
            let encoded = encode(s);
            assert!(!encoded.contains(&0), "{s:?} must not contain raw NUL");
            assert_eq!(decode(&encoded).unwrap(), s);
        }
        assert_eq!(encode("\0"), [0xC0, 0x80]);
        assert_eq!(encode("😀").len(), 6, "supplementary chars are two 3-byte surrogates");
    }

    #[test]
    fn preserves_lone_surrogates_in_utf16() {
        let units = [0x61, 0xD800, 0x62];
        assert_eq!(decode_utf16(&encode_utf16(&units)).unwrap(), units);
    }

    #[test]
    fn rejects_malformed_input() {
        assert!(decode(&[0x00]).is_err());
        assert!(decode(&[0xC0]).is_err());
        assert!(decode(&[0xE0, 0x80, 0x41]).is_err());
        assert!(decode(&[0xF0, 0x9F, 0x98, 0x80]).is_err());
    }
}
