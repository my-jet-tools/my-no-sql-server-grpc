use std::sync::Arc;

use my_no_sql_grpc_core::db::DbRow;

use super::binary_reader::{BinaryReader, write_u32_prefixed_bytes};

// The body of one slot - everything the server keeps about a single partition.
//
//   [0]     format   (u8)   1 = body as it is, 2 = body zstd-compressed
//   [1..]   body
//
// body:
//   rows_count (u32 LE)
//   per row:  schema_id (u64 LE)  row_len (u32 LE)  row
//
// `row` is the entity in its emit form - the client's bytes with TimeStamp and
// Expires appended - so a restored row needs no fixing up and re-parses into
// exactly what was stored. `schema_id` is per row on purpose: after a client
// changes its entity, old and new rows live in the same partition and each
// still names the schema it was written with.
//
// The format byte sits OUTSIDE the compressed area, so reading never has to
// guess whether the body was compressed.

const FORMAT_RAW: u8 = 1;
const FORMAT_ZSTD: u8 = 2;

/// zstd level 3 is the library default - the usual balance of ratio and speed.
const ZSTD_LEVEL: i32 = 3;

pub struct PersistedRow {
    pub schema_id: u64,
    pub row: Vec<u8>,
}

pub fn serialize(db_rows: &[Arc<DbRow>], compress: bool) -> Vec<u8> {
    let mut body = Vec::new();

    body.extend_from_slice(&(db_rows.len() as u32).to_le_bytes());

    for db_row in db_rows {
        body.extend_from_slice(&db_row.get_schema_id().to_le_bytes());
        write_u32_prefixed_bytes(&mut body, &db_row.to_vec());
    }

    if !compress {
        let mut result = Vec::with_capacity(body.len() + 1);
        result.push(FORMAT_RAW);
        result.extend_from_slice(&body);
        return result;
    }

    let compressed =
        zstd::encode_all(body.as_slice(), ZSTD_LEVEL).expect("zstd compress of a partition failed");

    let mut result = Vec::with_capacity(compressed.len() + 1);
    result.push(FORMAT_ZSTD);
    result.extend_from_slice(&compressed);
    result
}

pub fn deserialize(payload: &[u8]) -> Result<Vec<PersistedRow>, String> {
    let Some(format) = payload.first().copied() else {
        return Err("partition payload is empty".to_string());
    };

    let body = match format {
        FORMAT_RAW => payload[1..].to_vec(),
        FORMAT_ZSTD => zstd::decode_all(&payload[1..])
            .map_err(|err| format!("can not zstd-decompress the partition: {err}"))?,
        other => return Err(format!("unknown partition payload format {other}")),
    };

    let mut reader = BinaryReader::new(&body);
    let rows_count = reader.read_u32()? as usize;

    // The count comes out of the payload, and a payload can be one an operator
    // uploaded as a backup, so it does not get to size an allocation: a failed
    // allocation is an abort nobody can catch, while a loop over a body which is
    // not there stops on the first read with nothing behind it.
    let mut result = Vec::new();

    for _ in 0..rows_count {
        let schema_id = reader.read_u64()?;
        let row = reader.read_u32_prefixed_bytes()?;
        result.push(PersistedRow { schema_id, row });
    }

    if !reader.is_eof() {
        return Err("partition payload has trailing bytes after its rows".to_string());
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use my_no_sql_grpc_abstractions::db_entity::{ParsedEntity, consts, write_varint};
    use rust_extensions::date_time::DateTimeAsMicroseconds;

    use super::*;

    fn build_row(row_key: &str, schema_id: u64) -> Arc<DbRow> {
        let mut src = Vec::new();

        for (field_no, value) in [(1u32, "pk"), (2, row_key), (5, "payload")] {
            write_varint(
                &mut src,
                u64::from(field_no) << 3 | u64::from(consts::WIRE_TYPE_LEN),
            );
            write_varint(&mut src, value.len() as u64);
            src.extend_from_slice(value.as_bytes());
        }

        let parsed = ParsedEntity::parse(&src).unwrap();

        Arc::new(DbRow::new(
            parsed,
            schema_id,
            DateTimeAsMicroseconds::new(1_700_000_000_000_000),
        ))
    }

    fn round_trip(compress: bool) {
        let rows = vec![build_row("rk-1", 7), build_row("rk-2", 9)];

        let payload = serialize(&rows, compress);
        let restored = deserialize(&payload).unwrap();

        assert_eq!(restored.len(), 2);

        for (source, restored) in rows.iter().zip(restored.iter()) {
            assert_eq!(source.get_schema_id(), restored.schema_id);
            assert_eq!(source.to_vec(), restored.row);

            // The stored bytes must be a complete entity on their own.
            let parsed = ParsedEntity::parse(&restored.row).unwrap();
            assert_eq!(parsed.get_row_key(), source.get_row_key());
            assert_eq!(parsed.time_stamp, Some(1_700_000_000_000_000));
        }
    }

    #[test]
    fn raw_round_trip() {
        round_trip(false);
    }

    #[test]
    fn compressed_round_trip() {
        round_trip(true);
    }

    #[test]
    fn empty_partition_round_trips() {
        let payload = serialize(&[], true);
        assert!(deserialize(&payload).unwrap().is_empty());
    }

    #[test]
    fn truncated_payload_is_reported_not_panicked() {
        let rows = vec![build_row("rk-1", 7)];
        let mut payload = serialize(&rows, false);
        payload.truncate(payload.len() - 4);

        assert!(deserialize(&payload).is_err());
    }

    /// A blob arrives from an uploaded backup as well as from a page-file, so a
    /// row count nobody wrote must cost nothing.
    #[test]
    fn a_row_count_with_no_rows_behind_it_is_reported_not_allocated() {
        assert!(deserialize(&[FORMAT_RAW, 0xFF, 0xFF, 0xFF, 0xFF]).is_err());
    }

    #[test]
    fn unknown_format_is_reported() {
        assert!(deserialize(&[99, 1, 2, 3]).is_err());
    }
}
