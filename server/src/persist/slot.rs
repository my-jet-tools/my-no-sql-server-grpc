// Binary layout of one slot inside a size-class page-file. A slot is
// self-describing (carries its table + partition key), so recovery is a pure
// scan of the page-files - there is no separate key->location index on disk.
//
//   [0..4)    crc32      (u32 LE)  over bytes [4 .. 16 + body_len)
//   [4..12)   version    (u64 LE)  monotonic write counter (seeded from the
//                                  max on-disk version + 1 at scan); higher
//                                  wins on a duplicate after a crash
//                                  mid-relocation
//   [12..16)  body_len   (u32 LE)  length of body; 0 => slot is free
//   [16..)    body       table_len(u16) pk_len(u16) table pk payload
//   [...]     padding    zeroed up to the size class
//
// Both key lengths are u16, so neither key can be longer than `MAX_KEY_LEN` -
// a longer one is refused when it is written, see `assert_keys_fit`.
//
// In-place overwrite of a multi-sector slot is not power-loss atomic; the crc
// detects a torn write on recovery so a broken slot is skipped, never loaded.

/// crc(4) + version(8) + body_len(4).
pub const SLOT_PREFIX_LEN: usize = 16;
/// table_len(2) + pk_len(2), at the start of the body.
const KEY_LEN_FIELDS: usize = 4;
/// Fixed per-slot overhead before the keys and the payload.
pub const SLOT_OVERHEAD: usize = SLOT_PREFIX_LEN + KEY_LEN_FIELDS;

/// The longest table name and partition key a slot can carry: both lengths are
/// written as a `u16`, so this is the format's own limit and not a policy on top
/// of it.
pub const MAX_KEY_LEN: usize = u16::MAX as usize;

/// Total bytes a slot needs to hold this partition's payload.
pub fn slot_bytes_needed(table_name: &str, partition_key: &str, payload_len: usize) -> usize {
    assert_keys_fit(table_name, partition_key);

    SLOT_OVERHEAD + table_name.len() + partition_key.len() + payload_len
}

/// A key longer than the length field is refused here, where it is written,
/// rather than survived at recovery: `as u16` would store it truncated, the crc
/// is computed over the buffer as written, so nothing on the way back says
/// anything is wrong - the slot decodes into a shortened key with the rest of
/// itself glued onto the front of the payload, and the next start up either
/// panics on a payload format it does not know or, with `SkipBrokenPartitions`,
/// drops the partition without a word. Losing the write is the only outcome of
/// the three which is visible when it happens.
fn assert_keys_fit(table_name: &str, partition_key: &str) {
    assert!(
        table_name.len() <= MAX_KEY_LEN,
        "slot: the table name is {} bytes and a slot carries at most {MAX_KEY_LEN}",
        table_name.len()
    );

    assert!(
        partition_key.len() <= MAX_KEY_LEN,
        "slot: the partition key of '{table_name}' is {} bytes and a slot carries at most {MAX_KEY_LEN}",
        partition_key.len()
    );
}

/// Builds a full `size_class`-byte slot buffer (zero-padded) ready to be
/// written at the slot offset.
pub fn encode_slot(
    size_class: u32,
    version: u64,
    table_name: &str,
    partition_key: &str,
    payload: &[u8],
) -> Vec<u8> {
    assert_keys_fit(table_name, partition_key);

    let body_len = KEY_LEN_FIELDS + table_name.len() + partition_key.len() + payload.len();
    let mut buf = vec![0u8; size_class as usize];

    buf[4..12].copy_from_slice(&version.to_le_bytes());
    buf[12..16].copy_from_slice(&(body_len as u32).to_le_bytes());

    let mut pos = SLOT_PREFIX_LEN;
    buf[pos..pos + 2].copy_from_slice(&(table_name.len() as u16).to_le_bytes());
    pos += 2;
    buf[pos..pos + 2].copy_from_slice(&(partition_key.len() as u16).to_le_bytes());
    pos += 2;
    buf[pos..pos + table_name.len()].copy_from_slice(table_name.as_bytes());
    pos += table_name.len();
    buf[pos..pos + partition_key.len()].copy_from_slice(partition_key.as_bytes());
    pos += partition_key.len();
    buf[pos..pos + payload.len()].copy_from_slice(payload);
    pos += payload.len();

    let crc = crc32fast::hash(&buf[4..pos]);
    buf[0..4].copy_from_slice(&crc.to_le_bytes());

    buf
}

/// A successfully decoded, occupied slot.
pub struct OccupiedSlot {
    pub version: u64,
    pub table_name: String,
    pub partition_key: String,
    pub payload: Vec<u8>,
}

/// Result of decoding the bytes of one slot.
pub enum SlotState {
    /// `body_len == 0` - slot is empty / freed and available for reuse.
    Free,
    Occupied(OccupiedSlot),
    /// Length or crc check failed - a torn / corrupted write.
    Corrupt,
}

fn read_u32(src: &[u8]) -> u32 {
    u32::from_le_bytes(src.try_into().unwrap())
}

fn read_u16(src: &[u8]) -> u16 {
    u16::from_le_bytes(src.try_into().unwrap())
}

/// Decodes one slot from its full byte buffer (`bytes.len()` must equal the
/// size class).
pub fn decode_slot(bytes: &[u8]) -> SlotState {
    if bytes.len() < SLOT_PREFIX_LEN {
        return SlotState::Corrupt;
    }

    let crc_stored = read_u32(&bytes[0..4]);
    let version = u64::from_le_bytes(bytes[4..12].try_into().unwrap());
    let body_len = read_u32(&bytes[12..16]) as usize;

    if body_len == 0 {
        return SlotState::Free;
    }

    let end = SLOT_PREFIX_LEN + body_len;
    if end > bytes.len() {
        return SlotState::Corrupt;
    }

    if crc32fast::hash(&bytes[4..end]) != crc_stored {
        return SlotState::Corrupt;
    }

    let body = &bytes[SLOT_PREFIX_LEN..end];
    if body.len() < KEY_LEN_FIELDS {
        return SlotState::Corrupt;
    }

    let table_len = read_u16(&body[0..2]) as usize;
    let pk_len = read_u16(&body[2..4]) as usize;

    let mut pos = KEY_LEN_FIELDS;
    if pos + table_len + pk_len > body.len() {
        return SlotState::Corrupt;
    }

    let table_bytes = &body[pos..pos + table_len];
    pos += table_len;
    let pk_bytes = &body[pos..pos + pk_len];
    pos += pk_len;
    let payload = body[pos..].to_vec();

    let (Ok(table_name), Ok(partition_key)) = (
        String::from_utf8(table_bytes.to_vec()),
        String::from_utf8(pk_bytes.to_vec()),
    ) else {
        return SlotState::Corrupt;
    };

    SlotState::Occupied(OccupiedSlot {
        version,
        table_name,
        partition_key,
        payload,
    })
}

#[cfg(test)]
mod tests {
    use super::super::size_class::size_class_for;
    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        let payload = vec![1u8, 2, 3, 250, 0, 7];
        let needed = slot_bytes_needed("my-table", "pk-42", payload.len());
        let size_class = size_class_for(needed);

        let buf = encode_slot(size_class, 12345, "my-table", "pk-42", &payload);
        assert_eq!(buf.len(), size_class as usize);

        match decode_slot(&buf) {
            SlotState::Occupied(slot) => {
                assert_eq!(slot.version, 12345);
                assert_eq!(slot.table_name, "my-table");
                assert_eq!(slot.partition_key, "pk-42");
                assert_eq!(slot.payload, payload);
            }
            _ => panic!("expected occupied slot"),
        }
    }

    #[test]
    fn zeroed_slot_is_free() {
        let buf = vec![0u8; 512];
        assert!(matches!(decode_slot(&buf), SlotState::Free));
    }

    #[test]
    fn torn_payload_is_corrupt() {
        let payload = vec![10u8; 100];
        let size_class = size_class_for(slot_bytes_needed("t", "p", payload.len()));
        let mut buf = encode_slot(size_class, 1, "t", "p", &payload);
        // Flip a byte inside the crc-covered body without fixing the crc.
        buf[SLOT_PREFIX_LEN + 10] ^= 0xFF;
        assert!(matches!(decode_slot(&buf), SlotState::Corrupt));
    }

    /// The longest key the length field can describe is still a key.
    #[test]
    fn a_key_of_the_whole_length_field_roundtrips() {
        let partition_key = "k".repeat(MAX_KEY_LEN);
        let size_class = size_class_for(slot_bytes_needed("t", &partition_key, 0));

        match decode_slot(&encode_slot(size_class, 1, "t", &partition_key, &[])) {
            SlotState::Occupied(slot) => assert_eq!(slot.partition_key, partition_key),
            _ => panic!("expected occupied slot"),
        }
    }

    /// It would otherwise be written truncated, pass the crc, and come back at
    /// recovery as a shorter key with the rest of itself in front of the
    /// payload.
    #[test]
    #[should_panic(expected = "a slot carries at most")]
    fn a_partition_key_longer_than_the_length_field_is_refused() {
        encode_slot(262_144, 1, "t", &"k".repeat(MAX_KEY_LEN + 1), &[]);
    }

    #[test]
    #[should_panic(expected = "a slot carries at most")]
    fn a_table_name_longer_than_the_length_field_is_refused() {
        slot_bytes_needed(&"t".repeat(MAX_KEY_LEN + 1), "p", 0);
    }

    #[test]
    fn empty_payload_roundtrips() {
        let size_class = size_class_for(slot_bytes_needed("t", "p", 0));
        let buf = encode_slot(size_class, 9, "t", "p", &[]);
        match decode_slot(&buf) {
            SlotState::Occupied(slot) => assert!(slot.payload.is_empty()),
            _ => panic!("expected occupied slot"),
        }
    }
}
