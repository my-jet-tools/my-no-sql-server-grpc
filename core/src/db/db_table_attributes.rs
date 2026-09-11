use std::sync::Arc;

use ahash::AHashMap;
use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::schemas::EntitySchema;

/// Everything a table is, apart from its rows.
///
/// The schemas are here rather than beside the tables because a restart has to
/// bring back the shape and the data by one act: the metadata of a table is
/// written before any of its partitions and both go through the same queue, so
/// "rows on disk whose schema is not" stops being a state this server can reach.
#[derive(Clone)]
pub struct DbTableAttributes {
    pub persist: bool,
    pub max_partitions_amount: Option<usize>,
    pub max_rows_per_partition_amount: Option<usize>,
    pub created: DateTimeAsMicroseconds,
    /// Every schema this table's rows were written under, by id. A row keeps its
    /// own id, so rows of several entity versions live side by side and each is
    /// shown through the shape it arrived with.
    ///
    /// Behind an `Arc` because it is read on every write and replaced almost
    /// never - cloning the attributes must not clone the schemas with them.
    pub schemas: Arc<AHashMap<u64, Arc<EntitySchema>>>,
}

impl DbTableAttributes {
    pub fn create_default() -> Self {
        Self {
            persist: true,
            max_partitions_amount: None,
            max_rows_per_partition_amount: None,
            created: DateTimeAsMicroseconds::now(),
            schemas: Arc::default(),
        }
    }
}

impl Default for DbTableAttributes {
    fn default() -> Self {
        Self::create_default()
    }
}

/// Written by hand so the schemas show up as a count. They are the declared
/// shape of every entity version this table has seen, and a `{:?}` which spilled
/// all of it would bury whatever the line was actually about.
impl std::fmt::Debug for DbTableAttributes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DbTableAttributes")
            .field("persist", &self.persist)
            .field("max_partitions_amount", &self.max_partitions_amount)
            .field(
                "max_rows_per_partition_amount",
                &self.max_rows_per_partition_amount,
            )
            .field("created", &self.created)
            .field("schemas", &self.schemas.len())
            .finish()
    }
}
