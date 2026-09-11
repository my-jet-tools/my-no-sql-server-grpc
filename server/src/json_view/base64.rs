const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding - the encoding the protobuf JSON mapping uses
/// for a `bytes` field.
pub fn encode(src: &[u8]) -> String {
    let mut result = String::with_capacity(src.len().div_ceil(3) * 4);

    for chunk in src.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;

        let triple = (b0 << 16) | (b1 << 8) | b2;

        result.push(ALPHABET[(triple >> 18) as usize & 0x3F] as char);
        result.push(ALPHABET[(triple >> 12) as usize & 0x3F] as char);

        if chunk.len() > 1 {
            result.push(ALPHABET[(triple >> 6) as usize & 0x3F] as char);
        } else {
            result.push('=');
        }

        if chunk.len() > 2 {
            result.push(ALPHABET[triple as usize & 0x3F] as char);
        } else {
            result.push('=');
        }
    }

    result
}

/// The way back, for a `bytes` field arriving as JSON.
///
/// Padding is optional on the way in and whitespace is not: this reads what a
/// caller pasted out of a rendered row, and that is exactly what `encode` above
/// produces. Anything else is refused rather than guessed at - a byte string
/// silently shortened by one unreadable character is a byte string nobody can
/// account for.
pub fn decode(src: &str) -> Result<Vec<u8>, String> {
    let src = src.trim_end_matches('=');

    let mut result = Vec::with_capacity(src.len() / 4 * 3);
    let mut accumulator: u32 = 0;
    let mut bits = 0u32;

    for symbol in src.bytes() {
        let Some(value) = position(symbol) else {
            return Err(format!(
                "'{}' is not base64",
                symbol.escape_ascii().to_string().as_str()
            ));
        };

        accumulator = (accumulator << 6) | u32::from(value);
        bits += 6;

        if bits >= 8 {
            bits -= 8;
            result.push((accumulator >> bits) as u8);
        }
    }

    // 6 leftover bits is one base64 symbol on its own, which no encoder emits.
    if bits >= 6 {
        return Err("the base64 value is cut off".to_string());
    }

    Ok(result)
}

fn position(symbol: u8) -> Option<u8> {
    ALPHABET
        .iter()
        .position(|itm| *itm == symbol)
        .map(|itm| itm as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `encode` wrote comes back, which is the only round trip that has to
    /// hold: a `bytes` field is rendered by one and read by the other.
    #[test]
    fn what_was_encoded_decodes_back() {
        for value in [
            b"".as_slice(),
            b"f",
            b"fo",
            b"foo",
            b"foob",
            b"fooba",
            b"foobar",
            &[0xFF, 0xFE, 0xFD],
            &[0x00, 0x00],
        ] {
            assert_eq!(decode(&encode(value)).unwrap(), value, "{value:?}");
        }
    }

    #[test]
    fn padding_is_optional_and_rubbish_is_refused() {
        assert_eq!(decode("Zm9vYmFy").unwrap(), b"foobar");
        assert_eq!(decode("Zm9v").unwrap(), b"foo");
        // Same value, written without its padding.
        assert_eq!(decode("Zg").unwrap(), b"f");

        assert!(decode("Zm9v YmFy").is_err());
        assert!(decode("Zm9-").is_err());
        // One symbol on its own carries six bits and no byte.
        assert!(decode("Z").is_err());
    }

    #[test]
    fn known_vectors() {
        assert_eq!(encode(b""), "");
        assert_eq!(encode(b"f"), "Zg==");
        assert_eq!(encode(b"fo"), "Zm8=");
        assert_eq!(encode(b"foo"), "Zm9v");
        assert_eq!(encode(b"foob"), "Zm9vYg==");
        assert_eq!(encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(encode(&[0xFF, 0xFE, 0xFD]), "//79");
    }
}
