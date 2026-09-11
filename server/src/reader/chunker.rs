use std::sync::Arc;

use ahash::AHashMap;
use my_no_sql_grpc_core::db::{DbRow, PartitionRowKeys};

use super::PartitionRows;

/// A batch is cut by record count, with a ceiling on bytes as well: rows differ
/// in size by orders of magnitude between tables, so a chunk of "only a thousand
/// records" can still be too large to put into one message.
pub const MAX_ROWS_PER_CHUNK: usize = 1000;
pub const MAX_CHUNK_SIZE: usize = 1024 * 1024;

pub fn split_rows(rows: Vec<Arc<DbRow>>) -> Vec<Vec<Arc<DbRow>>> {
    let mut result = Vec::new();

    let mut chunk = Vec::new();
    let mut chunk_size = 0;

    for db_row in rows {
        chunk_size += db_row.get_content_size();
        chunk.push(db_row);

        if chunk.len() >= MAX_ROWS_PER_CHUNK || chunk_size >= MAX_CHUNK_SIZE {
            result.push(std::mem::take(&mut chunk));
            chunk_size = 0;
        }
    }

    if !chunk.is_empty() {
        result.push(chunk);
    }

    result
}

/// The same cut, for the rows of `InitPartitions`: they are grouped by partition
/// first, and the cut is then made wherever the limits say - inside a partition
/// too, if one partition alone is bigger than a chunk. That is safe because the
/// reader accumulates the whole batch until its `End` before applying anything,
/// so a partition split across two chunks arrives whole all the same.
pub fn split_partition_rows(rows: Vec<Arc<DbRow>>) -> Vec<Vec<PartitionRows>> {
    let mut result = Vec::new();

    let mut chunk: Vec<PartitionRows> = Vec::new();
    let mut chunk_rows = 0;
    let mut chunk_size = 0;

    for group in group_by_partition(rows) {
        for db_row in group.rows {
            chunk_rows += 1;
            chunk_size += db_row.get_content_size();

            match chunk.last_mut() {
                Some(last) if last.partition_key == group.partition_key => last.rows.push(db_row),
                _ => chunk.push(PartitionRows {
                    partition_key: group.partition_key.clone(),
                    rows: vec![db_row],
                }),
            }

            if chunk_rows >= MAX_ROWS_PER_CHUNK || chunk_size >= MAX_CHUNK_SIZE {
                result.push(std::mem::take(&mut chunk));
                chunk_rows = 0;
                chunk_size = 0;
            }
        }
    }

    if !chunk.is_empty() {
        result.push(chunk);
    }

    result
}

/// The same cut for a delete: what travels are keys rather than rows, so only
/// their count is worth counting. Splitting one partition's keys across two
/// chunks is safe for the same reason - the reader glues the batch back together
/// at its `End`.
pub fn split_partition_row_keys(partitions: Vec<PartitionRowKeys>) -> Vec<Vec<PartitionRowKeys>> {
    let mut result = Vec::new();

    let mut chunk: Vec<PartitionRowKeys> = Vec::new();
    let mut chunk_rows = 0;

    for partition in partitions {
        for row_key in partition.row_keys {
            chunk_rows += 1;

            match chunk.last_mut() {
                Some(last) if last.partition_key == partition.partition_key => {
                    last.row_keys.push(row_key)
                }
                _ => chunk.push(PartitionRowKeys {
                    partition_key: partition.partition_key.clone(),
                    row_keys: vec![row_key],
                }),
            }

            if chunk_rows >= MAX_ROWS_PER_CHUNK {
                result.push(std::mem::take(&mut chunk));
                chunk_rows = 0;
            }
        }
    }

    if !chunk.is_empty() {
        result.push(chunk);
    }

    result
}

/// Groups rows by partition, keeping both the order the partitions were first
/// seen in and the order of the rows inside each of them - a batch which wrote
/// the same key twice has to reach the reader in the order it was written.
fn group_by_partition(rows: Vec<Arc<DbRow>>) -> Vec<PartitionRows> {
    let mut result: Vec<PartitionRows> = Vec::new();
    let mut at: AHashMap<String, usize> = AHashMap::new();

    for db_row in rows {
        match at.get(db_row.get_partition_key()).copied() {
            Some(index) => result[index].rows.push(db_row),
            None => {
                let partition_key = db_row.get_partition_key().to_string();
                at.insert(partition_key.clone(), result.len());
                result.push(PartitionRows {
                    partition_key,
                    rows: vec![db_row],
                });
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use my_no_sql_grpc_core::db_entity::{ParsedEntity, consts, write_varint};
    use rust_extensions::date_time::DateTimeAsMicroseconds;

    use super::*;

    fn build_row(payload: &str) -> Arc<DbRow> {
        build_row_of("pk", "rk", payload)
    }

    fn build_row_of(partition_key: &str, row_key: &str, payload: &str) -> Arc<DbRow> {
        let mut src = Vec::new();

        for (field_no, value) in [(1u32, partition_key), (2, row_key), (5, payload)] {
            write_varint(
                &mut src,
                u64::from(field_no) << 3 | u64::from(consts::WIRE_TYPE_LEN),
            );
            write_varint(&mut src, value.len() as u64);
            src.extend_from_slice(value.as_bytes());
        }

        let parsed = ParsedEntity::parse(&src).unwrap();
        Arc::new(DbRow::new(parsed, 1, DateTimeAsMicroseconds::new(1)))
    }

    #[test]
    fn a_small_batch_stays_one_chunk() {
        let rows: Vec<Arc<DbRow>> = (0..10).map(|_| build_row("x")).collect();
        let chunks = split_rows(rows);

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].len(), 10);
    }

    #[test]
    fn an_empty_batch_produces_no_chunks() {
        assert!(split_rows(Vec::new()).is_empty());
    }

    #[test]
    fn the_record_count_cuts_the_batch() {
        let rows: Vec<Arc<DbRow>> = (0..MAX_ROWS_PER_CHUNK * 2 + 1)
            .map(|_| build_row("x"))
            .collect();

        let chunks = split_rows(rows);

        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].len(), MAX_ROWS_PER_CHUNK);
        assert_eq!(chunks[1].len(), MAX_ROWS_PER_CHUNK);
        assert_eq!(chunks[2].len(), 1);
    }

    /// Few records, but each of them huge - the byte ceiling has to cut it well
    /// before the record count would.
    #[test]
    fn the_byte_ceiling_cuts_the_batch_too() {
        let big = "x".repeat(300 * 1024);
        let rows: Vec<Arc<DbRow>> = (0..10).map(|_| build_row(&big)).collect();

        let chunks = split_rows(rows);

        assert!(chunks.len() > 1);
        for chunk in &chunks {
            assert!(chunk.len() < MAX_ROWS_PER_CHUNK);
        }
    }

    #[test]
    fn rows_are_grouped_by_partition_in_the_order_they_arrived() {
        let rows = vec![
            build_row_of("b", "rk-1", "x"),
            build_row_of("a", "rk-1", "x"),
            build_row_of("b", "rk-2", "x"),
        ];

        let chunks = split_partition_rows(rows);

        assert_eq!(chunks.len(), 1);

        let keys: Vec<&str> = chunks[0]
            .iter()
            .map(|itm| itm.partition_key.as_str())
            .collect();
        assert_eq!(keys, vec!["b", "a"]);

        assert_eq!(chunks[0][0].rows.len(), 2);
        assert_eq!(chunks[0][1].rows.len(), 1);
    }

    #[test]
    fn an_empty_batch_produces_no_partition_chunks() {
        assert!(split_partition_rows(Vec::new()).is_empty());
    }

    /// One partition bigger than a chunk has to be cut, and the pieces keep
    /// naming it - the reader glues them back together at the `End`.
    #[test]
    fn a_partition_bigger_than_a_chunk_is_cut_and_still_named_by_every_piece() {
        let rows: Vec<Arc<DbRow>> = (0..MAX_ROWS_PER_CHUNK + 1)
            .map(|no| build_row_of("one", &format!("rk-{no}"), "x"))
            .collect();

        let chunks = split_partition_rows(rows);

        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0][0].partition_key, "one");
        assert_eq!(chunks[0][0].rows.len(), MAX_ROWS_PER_CHUNK);
        assert_eq!(chunks[1][0].partition_key, "one");
        assert_eq!(chunks[1][0].rows.len(), 1);
    }
}
