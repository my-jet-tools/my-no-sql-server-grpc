/// A batch is cut by row count with a ceiling on bytes as well, exactly the way
/// the server cuts what it sends: entities differ in size by orders of magnitude
/// between tables, so "only a thousand rows" can still be too much for one
/// message.
///
/// The cut is transport and nothing else. The server accumulates the whole
/// stream and applies it in one entry into the table, so where a row lands
/// changes nothing about what the batch means.
pub const MAX_ROWS_PER_MESSAGE: usize = 1000;
pub const MAX_MESSAGE_SIZE: usize = 1024 * 1024;

pub(crate) fn cut_rows(rows: impl Iterator<Item = Vec<u8>>) -> Vec<Vec<Vec<u8>>> {
    let mut result = Vec::new();

    let mut message = Vec::new();
    let mut message_size = 0;

    for row in rows {
        message_size += row.len();
        message.push(row);

        if message.len() >= MAX_ROWS_PER_MESSAGE || message_size >= MAX_MESSAGE_SIZE {
            result.push(std::mem::take(&mut message));
            message_size = 0;
        }
    }

    if !message.is_empty() {
        result.push(message);
    }

    result
}

/// A batch which brought no rows is still an operation - `CleanTableAndInsert`
/// with nothing in it is a clean - so it has to reach the server as one message
/// rather than as no stream at all.
pub(crate) fn cut_rows_at_least_once(rows: impl Iterator<Item = Vec<u8>>) -> Vec<Vec<Vec<u8>>> {
    let result = cut_rows(rows);

    if result.is_empty() {
        return vec![Vec::new()];
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_small_batch_stays_one_message() {
        let cut = cut_rows((0..10).map(|_| vec![1u8, 2, 3]));

        assert_eq!(cut.len(), 1);
        assert_eq!(cut[0].len(), 10);
    }

    #[test]
    fn the_row_count_cuts_the_batch() {
        let cut = cut_rows((0..MAX_ROWS_PER_MESSAGE * 2 + 1).map(|_| vec![1u8]));

        assert_eq!(cut.len(), 3);
        assert_eq!(cut[0].len(), MAX_ROWS_PER_MESSAGE);
        assert_eq!(cut[2].len(), 1);
    }

    #[test]
    fn the_byte_ceiling_cuts_the_batch_too() {
        let cut = cut_rows((0..10).map(|_| vec![0u8; 300 * 1024]));

        assert!(cut.len() > 1);
        for message in &cut {
            assert!(message.len() < MAX_ROWS_PER_MESSAGE);
        }
    }

    /// An empty batch is not an empty stream: the stream is what names the table
    /// and the mode, so it has to carry at least the one message.
    #[test]
    fn an_empty_batch_still_becomes_one_message() {
        assert!(cut_rows(std::iter::empty()).is_empty());

        let cut = cut_rows_at_least_once(std::iter::empty());
        assert_eq!(cut.len(), 1);
        assert!(cut[0].is_empty());
    }
}
