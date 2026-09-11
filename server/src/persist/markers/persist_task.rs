/// One unit of work for the persist loop. A partition is the smallest thing this
/// server writes - a slot holds the whole partition, so there is nothing finer
/// to mark.
#[derive(Debug, PartialEq, Eq)]
pub enum PersistTask {
    TableMetadata {
        table_name: String,
    },
    /// Also covers "the partition is gone": the executor looks the partition up
    /// and deletes its slot when it is no longer in the table.
    Partition {
        table_name: String,
        partition_key: String,
    },
}
