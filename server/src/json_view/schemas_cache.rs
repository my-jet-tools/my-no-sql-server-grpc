use std::sync::Arc;

use ahash::AHashMap;
use arc_swap::ArcSwap;
use my_no_sql_grpc_core::schemas::EntitySchema;
use parking_lot::Mutex;

use super::SchemaIndex;

/// Resolved schemas, keyed by schema id.
///
/// Reading a schema and resolving every reference in it is far too much work to
/// redo per row, so one cache serves every table of every namespace. An id is
/// derived from the shape and a table refuses a second shape under an id it
/// already has, so two entries can only collide if a client made up a number -
/// and such a client is already showing its own rows under somebody else's
/// names. A blob that does not parse is cached as a miss too: retrying it on
/// every row would burn the same work for the same answer.
pub struct JsonSchemasCache {
    inner: ArcSwap<AHashMap<u64, Option<Arc<SchemaIndex>>>>,
    write_lock: Mutex<()>,
}

impl JsonSchemasCache {
    pub fn new() -> Self {
        Self {
            inner: ArcSwap::from_pointee(AHashMap::new()),
            write_lock: Mutex::new(()),
        }
    }

    pub fn get_or_build(&self, schema: &EntitySchema) -> Option<Arc<SchemaIndex>> {
        if let Some(cached) = self.inner.load().get(&schema.id) {
            return cached.clone();
        }

        let _guard = self.write_lock.lock();

        if let Some(cached) = self.inner.load().get(&schema.id) {
            return cached.clone();
        }

        let built = match SchemaIndex::build(&schema.schema) {
            Ok(index) => Some(Arc::new(index)),
            Err(err) => {
                my_logger::LOGGER.write_error(
                    "json_view",
                    format!("Can not use schema {}: {}", schema.id, err),
                    my_logger::LogEventCtx::new(),
                );
                None
            }
        };

        let mut map = self.inner.load().as_ref().clone();
        map.insert(schema.id, built.clone());
        self.inner.store(Arc::new(map));

        built
    }
}

impl Default for JsonSchemasCache {
    fn default() -> Self {
        Self::new()
    }
}
