use std::io::{Cursor, Read, Write};

use my_no_sql_grpc_core::db::DbTableAttributes;

use crate::persist::{TableMetadataFileContract, base64url};

// A backup is a zip of one namespace, laid out the way the namespace is:
//
//   <table>/.metadata               that table's attributes, schemas and all
//   <table>/<base64url(partition)>  the partition, as a partition blob
//
// `<table>` is the table's own name while that name can be a path segment, and
// `~<base64url(name)>` when it can not - see `encode_table_name`.
//
// A partition is already serialized as a self-contained blob for the page-files,
// so a backup does not need a second way to write one - it is an array of
// partitions, one zip entry each, with the metadata they can not be read
// without beside them. The schemas are part of that metadata, so a restored
// table is showable without a second entry to carry them.
//
// The blobs go in **uncompressed** and the zip deflates them. Zstd-ing each blob
// and then deflating the result would cost the compression twice and save it
// once, and the zip is what an operator opens with the tool they already have.

const METADATA_ENTRY: &str = ".metadata";

/// Marks a table name which had to be encoded to become a path segment. It is
/// not in the base64url alphabet, so a name carrying it is never one which does
/// not.
const ENCODED_TABLE_MARK: char = '~';

/// One table as a backup holds it.
pub struct BackupTableContent {
    pub table_name: String,
    pub attributes: Option<DbTableAttributes>,
    pub partitions: Vec<BackupPartitionContent>,
}

pub struct BackupPartitionContent {
    pub partition_key: String,
    /// Exactly what [`crate::persist::partition_blob::serialize`] makes.
    pub blob: Vec<u8>,
}

/// Everything one backup carries.
pub struct BackupContent {
    pub tables: Vec<BackupTableContent>,
}

/// Builds the zip of one namespace.
pub struct BackupZipBuilder {
    zip: zip::ZipWriter<Cursor<Vec<u8>>>,
}

impl BackupZipBuilder {
    pub fn new() -> Self {
        Self {
            zip: zip::ZipWriter::new(Cursor::new(Vec::new())),
        }
    }

    pub fn add_table(
        &mut self,
        table_name: &str,
        attributes: &DbTableAttributes,
    ) -> Result<(), String> {
        let contract: TableMetadataFileContract = attributes.into();

        let payload = serde_yaml::to_string(&contract)
            .map_err(|err| format!("can not write the attributes of {table_name}: {err}"))?;

        self.add_entry(
            &format!("{}/{METADATA_ENTRY}", encode_table_name(table_name)),
            payload.as_bytes(),
        )
    }

    pub fn add_partition(
        &mut self,
        table_name: &str,
        partition_key: &str,
        blob: &[u8],
    ) -> Result<(), String> {
        self.add_entry(
            &format!(
                "{}/{}",
                encode_table_name(table_name),
                base64url::encode(partition_key)
            ),
            blob,
        )
    }

    pub fn build(self) -> Result<Vec<u8>, String> {
        Ok(self
            .zip
            .finish()
            .map_err(|err| format!("can not close the backup: {err}"))?
            .into_inner())
    }

    fn add_entry(&mut self, name: &str, payload: &[u8]) -> Result<(), String> {
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);

        self.zip
            .start_file(name, options)
            .map_err(|err| format!("can not start {name} inside the backup: {err}"))?;

        self.zip
            .write_all(payload)
            .map_err(|err| format!("can not write {name} inside the backup: {err}"))
    }
}

impl Default for BackupZipBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Reads a backup back.
///
/// An entry whose name this build does not understand is skipped rather than
/// refused: a backup written by a newer server still restores what this one can
/// read instead of restoring nothing.
pub fn read(src: &[u8]) -> Result<BackupContent, String> {
    let mut archive = zip::ZipArchive::new(Cursor::new(src))
        .map_err(|err| format!("this is not a backup of this server: {err}"))?;

    let mut tables: Vec<BackupTableContent> = Vec::new();

    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|err| format!("can not read an entry of the backup: {err}"))?;

        let name = entry.name().to_string();

        let mut payload = Vec::new();
        entry
            .read_to_end(&mut payload)
            .map_err(|err| format!("can not read {name} out of the backup: {err}"))?;

        let Some((table_entry, rest)) = name.split_once('/') else {
            continue;
        };

        let table_name = decode_table_name(table_entry);
        let table = get_or_create(&mut tables, &table_name);

        if rest == METADATA_ENTRY {
            let contract: TableMetadataFileContract = serde_yaml::from_slice(&payload)
                .map_err(|err| format!("can not read the attributes of {table_name}: {err}"))?;

            table.attributes = Some(contract.into());
            continue;
        }

        table.partitions.push(BackupPartitionContent {
            partition_key: base64url::decode(rest)?,
            blob: payload,
        });
    }

    Ok(BackupContent { tables })
}

/// A table name is validated nowhere, so it can hold the `/` zip entries are
/// built out of - and one such name would otherwise make every other table in
/// the archive unreadable, because the name of a partition is decoded and a
/// decode which fails ends the whole read. A name which is already a path
/// segment is left alone: the point of writing a plain zip is that an operator
/// can open it and see which table is which.
fn encode_table_name(table_name: &str) -> String {
    if is_path_segment(table_name) {
        return table_name.to_string();
    }

    format!("{ENCODED_TABLE_MARK}{}", base64url::encode(table_name))
}

fn decode_table_name(table_entry: &str) -> String {
    let Some(encoded) = table_entry.strip_prefix(ENCODED_TABLE_MARK) else {
        return table_entry.to_string();
    };

    // A backup taken before table names were encoded could carry one which
    // starts with the mark and is not encoded at all, so a decode which does not
    // work is the name itself rather than the end of the whole archive.
    base64url::decode(encoded).unwrap_or_else(|_| table_entry.to_string())
}

/// Whether the name survives being one segment of a zip entry and coming back.
/// `..` is refused as well: the archive is only ever read into memory here, but
/// an operator unpacking it with an ordinary tool would have it step out of the
/// folder they unpacked into.
fn is_path_segment(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with(ENCODED_TABLE_MARK)
        && !name.contains(['/', '\\', '\0'])
        && name != "."
        && name != ".."
}

fn get_or_create<'s>(
    tables: &'s mut Vec<BackupTableContent>,
    table_name: &str,
) -> &'s mut BackupTableContent {
    if let Some(at) = tables.iter().position(|itm| itm.table_name == table_name) {
        return &mut tables[at];
    }

    tables.push(BackupTableContent {
        table_name: table_name.to_string(),
        attributes: None,
        partitions: Vec::new(),
    });

    tables.last_mut().unwrap()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ahash::AHashMap;
    use my_no_sql_grpc_core::schemas::EntitySchema;
    use rust_extensions::date_time::DateTimeAsMicroseconds;

    use super::*;

    fn schemas(schemas: Vec<EntitySchema>) -> Arc<AHashMap<u64, Arc<EntitySchema>>> {
        let mut result = AHashMap::new();

        for schema in schemas {
            result.insert(schema.id, Arc::new(schema));
        }

        Arc::new(result)
    }

    fn built() -> Vec<u8> {
        let mut builder = BackupZipBuilder::new();

        builder
            .add_table(
                "traders",
                &DbTableAttributes {
                    persist: true,
                    max_partitions_amount: Some(10),
                    max_rows_per_partition_amount: None,
                    created: DateTimeAsMicroseconds::new(777),
                    schemas: schemas(vec![EntitySchema::new(42, vec![1, 2, 3])]),
                },
            )
            .unwrap();

        // A key which would have had a path in it under standard base64.
        builder
            .add_partition("traders", "acc/1", &[9, 8, 7])
            .unwrap();

        builder.build().unwrap()
    }

    /// The schemas ride inside the table's metadata, so a table comes back out
    /// of an archive with the shapes its rows were written under and no entry of
    /// its own to carry them.
    #[test]
    fn a_backup_round_trips() {
        let content = read(&built()).unwrap();

        assert_eq!(content.tables.len(), 1);
        let table = &content.tables[0];
        assert_eq!(table.table_name, "traders");

        let attributes = table.attributes.as_ref().unwrap();
        assert!(attributes.persist);
        assert_eq!(attributes.max_partitions_amount, Some(10));
        assert_eq!(attributes.schemas.len(), 1);
        assert_eq!(attributes.schemas.get(&42).unwrap().schema, vec![1, 2, 3]);
        // A limit which is not set is not a limit of zero.
        assert_eq!(attributes.max_rows_per_partition_amount, None);

        assert_eq!(table.partitions.len(), 1);
        assert_eq!(table.partitions[0].partition_key, "acc/1");
        assert_eq!(table.partitions[0].blob, vec![9, 8, 7]);
    }

    /// It is an ordinary zip, so an ordinary zip tool sees the layout.
    #[test]
    fn the_entries_are_named_after_the_table_and_the_partition() {
        let bytes = built();
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes.as_slice())).unwrap();

        let names: Vec<String> = (0..archive.len())
            .map(|index| archive.by_index(index).unwrap().name().to_string())
            .collect();

        assert!(names.contains(&"traders/.metadata".to_string()));
        // One entry per partition, and none of them nested any deeper.
        assert!(names.iter().all(|name| name.matches('/').count() <= 1));
    }

    /// Table names are validated nowhere, so one of them holding a `/` must not
    /// take the tables next to it down with it.
    #[test]
    fn a_table_whose_name_is_a_path_does_not_cost_the_whole_archive() {
        let mut builder = BackupZipBuilder::new();

        builder
            .add_table(
                "eu/traders",
                &DbTableAttributes {
                    persist: true,
                    max_partitions_amount: None,
                    max_rows_per_partition_amount: None,
                    created: DateTimeAsMicroseconds::new(1),
                    schemas: Arc::default(),
                },
            )
            .unwrap();
        builder.add_partition("eu/traders", "acc-1", &[1]).unwrap();
        builder.add_partition("plain", "acc-2", &[2]).unwrap();

        let content = read(&builder.build().unwrap()).unwrap();

        let nested = content
            .tables
            .iter()
            .find(|itm| itm.table_name == "eu/traders")
            .unwrap();
        assert!(nested.attributes.is_some());
        assert_eq!(nested.partitions.len(), 1);
        assert_eq!(nested.partitions[0].partition_key, "acc-1");

        let plain = content
            .tables
            .iter()
            .find(|itm| itm.table_name == "plain")
            .unwrap();
        assert_eq!(plain.partitions.len(), 1);
    }

    /// An operator's archive of yesterday is still an archive.
    #[test]
    fn a_backup_written_before_table_names_were_encoded_still_reads() {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();

        zip.start_file("traders/.metadata", options).unwrap();
        zip.write_all(
            serde_yaml::to_string(&TableMetadataFileContract::from(&DbTableAttributes {
                persist: true,
                max_partitions_amount: None,
                max_rows_per_partition_amount: None,
                created: DateTimeAsMicroseconds::new(5),
                schemas: Arc::default(),
            }))
            .unwrap()
            .as_bytes(),
        )
        .unwrap();

        zip.start_file(format!("traders/{}", base64url::encode("acc-1")), options)
            .unwrap();
        zip.write_all(&[7]).unwrap();

        let content = read(&zip.finish().unwrap().into_inner()).unwrap();

        assert_eq!(content.tables.len(), 1);
        assert_eq!(content.tables[0].table_name, "traders");
        assert_eq!(content.tables[0].partitions[0].partition_key, "acc-1");
    }

    #[test]
    fn something_which_is_not_a_zip_is_refused() {
        assert!(read(b"hello").is_err());
        assert!(read(&[]).is_err());
    }

    #[test]
    fn a_truncated_backup_is_reported_not_panicked() {
        let mut bytes = built();
        bytes.truncate(bytes.len() / 2);

        assert!(read(&bytes).is_err());
    }
}
