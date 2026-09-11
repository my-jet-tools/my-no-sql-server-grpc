use std::sync::Arc;

use ahash::AHashMap;
use my_no_sql_grpc_abstractions::schemas::EntitySchema;
use my_no_sql_grpc_core::db::DbTableAttributes;
use rust_extensions::date_time::DateTimeAsMicroseconds;
use serde::{Deserialize, Serialize};

use super::base64url;

/// One entry of `tables.meta`. The `Option` fields are skipped when empty so the
/// file stays small, and each carries `#[serde(default)]` so an omitted field
/// reads back as `None`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TableMetadataFileContract {
    #[serde(rename = "Persist")]
    #[serde(default = "default_persist")]
    pub persist: bool,

    #[serde(rename = "MaxPartitionsAmount")]
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_partitions_amount: Option<usize>,

    #[serde(rename = "MaxRowsPerPartitionAmount")]
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_rows_per_partition_amount: Option<usize>,

    #[serde(rename = "Created")]
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,

    /// The schemas the table's rows were written under.
    ///
    /// They live in this file and not in one of their own because a file about
    /// exactly this table already exists, and because the queue hands the
    /// metadata of a table out before any of its partitions: the shape and the
    /// data are restored by one act, so a row whose schema is not on disk is not
    /// a state this server can reach.
    #[serde(rename = "Schemas")]
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub schemas: Vec<TableSchemaFileContract>,
}

/// One schema of one table. The bytes are base64url rather than a YAML list of
/// numbers: this file is opened by operators.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TableSchemaFileContract {
    #[serde(rename = "Id")]
    pub id: u64,
    #[serde(rename = "Schema")]
    pub schema: String,
}

fn default_persist() -> bool {
    true
}

impl From<&DbTableAttributes> for TableMetadataFileContract {
    fn from(src: &DbTableAttributes) -> Self {
        // Sorted by id, so that a table whose schemas did not change writes the
        // same file twice rather than two spellings of it.
        let mut schemas: Vec<TableSchemaFileContract> = src
            .schemas
            .values()
            .map(|schema| TableSchemaFileContract {
                id: schema.id,
                schema: base64url::encode_bytes(&schema.schema),
            })
            .collect();

        schemas.sort_by_key(|itm| itm.id);

        Self {
            persist: src.persist,
            max_partitions_amount: src.max_partitions_amount,
            max_rows_per_partition_amount: src.max_rows_per_partition_amount,
            created: Some(src.created.to_rfc3339()),
            schemas,
        }
    }
}

impl From<TableMetadataFileContract> for DbTableAttributes {
    fn from(src: TableMetadataFileContract) -> Self {
        let mut schemas = AHashMap::new();

        for schema in src.schemas.iter() {
            // A schema which does not decode is dropped rather than fatal: what
            // it costs is the ability to show the rows written under it, and
            // losing the table because one line of its metadata is unreadable
            // costs the rows themselves.
            let Ok(bytes) = base64url::decode_bytes(&schema.schema) else {
                println!(
                    "The schema {} of a table can not be read out of tables.meta. Rows written under it will not be rendered until a client writes it again.",
                    schema.id
                );
                continue;
            };

            schemas.insert(schema.id, Arc::new(EntitySchema::new(schema.id, bytes)));
        }

        Self {
            persist: src.persist,
            // A stored 0 means "no limit" - that is what the write side turns an
            // absent limit into, and a table capped at zero partitions would
            // delete itself on the first GC pass.
            max_partitions_amount: to_optional_limit(src.max_partitions_amount),
            max_rows_per_partition_amount: to_optional_limit(src.max_rows_per_partition_amount),
            created: match src.created.as_deref() {
                Some(created) => DateTimeAsMicroseconds::from_str(created)
                    .unwrap_or_else(DateTimeAsMicroseconds::now),
                None => DateTimeAsMicroseconds::now(),
            },
            schemas: Arc::new(schemas),
        }
    }
}

fn to_optional_limit(src: Option<usize>) -> Option<usize> {
    let value = src?;

    if value == 0 {
        return None;
    }

    Some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Compressed` was written into this file by every build before the
    /// attribute was cut. A server which refused to load because of a key it no
    /// longer knows would lose the table it describes, so the key has to be
    /// ignored rather than met with an error.
    #[test]
    fn a_file_written_before_compressed_was_cut_still_loads() {
        let contract: TableMetadataFileContract = serde_yaml::from_str(
            "Persist: true\nMaxPartitionsAmount: 9\nCompressed: true\nCreated: '2026-01-04T10:00:00+00:00'\n",
        )
        .unwrap();

        assert!(contract.persist);
        assert_eq!(contract.max_partitions_amount, Some(9));

        let attributes: DbTableAttributes = contract.into();
        assert_eq!(attributes.max_partitions_amount, Some(9));
    }

    /// ...and a new one does not write it back.
    #[test]
    fn what_is_written_now_does_not_mention_it() {
        let attributes = DbTableAttributes::create_default();
        let contract: TableMetadataFileContract = (&attributes).into();

        let yaml = serde_yaml::to_string(&contract).unwrap();

        assert!(!yaml.contains("Compressed"), "{yaml}");
        assert!(yaml.contains("Persist"), "{yaml}");
    }

    fn with_schemas(schemas: Vec<EntitySchema>) -> DbTableAttributes {
        let mut map = AHashMap::new();

        for schema in schemas {
            map.insert(schema.id, Arc::new(schema));
        }

        DbTableAttributes {
            schemas: Arc::new(map),
            ..DbTableAttributes::create_default()
        }
    }

    /// The schemas ride inside this file because the queue writes it before any
    /// partition of the table - so a restart brings the shape and the data back
    /// together.
    #[test]
    fn the_schemas_of_a_table_round_trip_through_its_metadata() {
        let attributes = with_schemas(vec![
            EntitySchema::new(9, vec![0, 1, 2, 250, 255]),
            EntitySchema::new(7, Vec::new()),
        ]);

        let contract: TableMetadataFileContract = (&attributes).into();
        let yaml = serde_yaml::to_string(&contract).unwrap();

        // Written in one line each, in id order: this file is read by people.
        assert!(yaml.contains("Schemas:"), "{yaml}");
        assert_eq!(contract.schemas[0].id, 7);
        assert_eq!(contract.schemas[1].id, 9);

        let restored: DbTableAttributes = serde_yaml::from_str::<TableMetadataFileContract>(&yaml)
            .unwrap()
            .into();

        assert_eq!(restored.schemas.len(), 2);
        assert_eq!(
            restored.schemas.get(&9).unwrap().schema,
            vec![0, 1, 2, 250, 255]
        );
        assert!(restored.schemas.get(&7).unwrap().schema.is_empty());
    }

    /// A table which never took a write has nothing to say here, and a key with
    /// an empty list under it would be one more thing to explain.
    #[test]
    fn a_table_with_no_schemas_does_not_write_the_key() {
        let contract: TableMetadataFileContract = (&DbTableAttributes::create_default()).into();
        let yaml = serde_yaml::to_string(&contract).unwrap();

        assert!(!yaml.contains("Schemas"), "{yaml}");

        // ...and a file written before this key existed reads back as a table
        // which has not been written to yet, rather than as a broken one.
        let restored: TableMetadataFileContract =
            serde_yaml::from_str("Persist: true\nCreated: '2026-01-04T10:00:00+00:00'\n").unwrap();

        assert!(restored.schemas.is_empty());
    }

    /// One unreadable line costs the rows written under that one schema. Costing
    /// the table would be the wrong trade: the rows themselves are still there
    /// and still served.
    #[test]
    fn a_schema_which_does_not_decode_does_not_cost_the_table() {
        let contract: TableMetadataFileContract = serde_yaml::from_str(
            "Persist: true\nSchemas:\n- Id: 7\n  Schema: 'not base64!'\n- Id: 9\n  Schema: 'AAEC'\n",
        )
        .unwrap();

        let attributes: DbTableAttributes = contract.into();

        assert!(attributes.persist);
        assert_eq!(attributes.schemas.len(), 1);
        assert_eq!(attributes.schemas.get(&9).unwrap().schema, vec![0, 1, 2]);
    }
}
