//! Base64 with the url alphabet and no padding.
//!
//! A partition key becomes a file name inside a backup zip, so the encoding has
//! to be one no path can be made out of. Standard base64 is not: `/` is in its
//! alphabet, and a key which encoded to one would turn `traders/<key>` into a
//! nested folder nobody meant. `-` and `_` take its place here, and the padding
//! is dropped because a file name does not need it.
//!
//! The same alphabet carries the schema bytes through `tables.meta`, which is
//! YAML: a `Vec<u8>` written as YAML is a list of three hundred numbers, and one
//! string on one line is what an operator opening that file should see.

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

pub fn encode(src: &str) -> String {
    encode_bytes(src.as_bytes())
}

pub fn encode_bytes(src: &[u8]) -> String {
    let mut result = String::with_capacity(src.len().div_ceil(3) * 4);

    for chunk in src.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = u32::from(*chunk.get(1).unwrap_or(&0));
        let b2 = u32::from(*chunk.get(2).unwrap_or(&0));

        let triple = (b0 << 16) | (b1 << 8) | b2;

        result.push(ALPHABET[(triple >> 18) as usize & 0x3F] as char);
        result.push(ALPHABET[(triple >> 12) as usize & 0x3F] as char);

        if chunk.len() > 1 {
            result.push(ALPHABET[(triple >> 6) as usize & 0x3F] as char);
        }

        if chunk.len() > 2 {
            result.push(ALPHABET[triple as usize & 0x3F] as char);
        }
    }

    result
}

pub fn decode(src: &str) -> Result<String, String> {
    String::from_utf8(decode_bytes(src)?)
        .map_err(|_| format!("'{src}' does not decode into a partition key"))
}

pub fn decode_bytes(src: &str) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::with_capacity(src.len() / 4 * 3);

    let mut buffer: u32 = 0;
    let mut collected = 0;

    for symbol in src.chars() {
        let Some(value) = ALPHABET.iter().position(|itm| *itm as char == symbol) else {
            return Err(format!("'{src}' is not something this server wrote"));
        };

        buffer = (buffer << 6) | value as u32;
        collected += 1;

        if collected == 4 {
            bytes.push((buffer >> 16) as u8);
            bytes.push((buffer >> 8) as u8);
            bytes.push(buffer as u8);
            buffer = 0;
            collected = 0;
        }
    }

    // What is left over is the tail of a group: two symbols carry one byte,
    // three carry two, and one on its own carries nothing that can be a byte.
    match collected {
        0 => {}
        2 => bytes.push((buffer >> 4) as u8),
        3 => {
            bytes.push((buffer >> 10) as u8);
            bytes.push((buffer >> 2) as u8);
        }
        _ => return Err(format!("'{src}' is not something this server wrote")),
    }

    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(src: &str) {
        let encoded = encode(src);

        assert!(
            !encoded.contains('/') && !encoded.contains('\\') && !encoded.contains('='),
            "'{encoded}' can not be a file name"
        );

        assert_eq!(decode(&encoded).unwrap(), src);
    }

    #[test]
    fn a_partition_key_survives_becoming_a_file_name() {
        for key in [
            "a",
            "ab",
            "abc",
            "abcd",
            "acc-1",
            "trader/account",
            "..",
            "ключ на кириллице",
            "with spaces and \\ backslashes",
            &"x".repeat(300),
        ] {
            round_trip(key);
        }
    }

    /// Schema bytes are not text, and they go through the same alphabet into
    /// `tables.meta`.
    #[test]
    fn arbitrary_bytes_survive_becoming_a_line_of_yaml() {
        for length in 0..40usize {
            let src: Vec<u8> = (0..length).map(|itm| (itm * 7 + 3) as u8).collect();
            assert_eq!(decode_bytes(&encode_bytes(&src)).unwrap(), src);
        }

        let all: Vec<u8> = (0..=255u8).collect();
        assert_eq!(decode_bytes(&encode_bytes(&all)).unwrap(), all);
    }

    #[test]
    fn something_which_is_not_an_encoded_key_is_refused() {
        assert!(decode("not base64!").is_err());
        // One symbol on its own is not the tail of any group.
        assert!(decode("A").is_err());
    }

    /// The whole reason for the url alphabet: a key which standard base64 would
    /// have encoded with a `/` in it.
    #[test]
    fn no_key_encodes_into_something_with_a_path_in_it() {
        for byte in 0..=255u8 {
            let key = format!("a{}", byte as char);
            assert!(!encode(&key).contains('/'));
        }
    }
}
