use std::sync::Arc;

use ahash::AHashMap;
use my_no_sql_grpc_core::db::{BulkWriteMode, DbRow, DbTable, GetRowsFilter};
use my_no_sql_grpc_core::db_entity::ParsedEntity;
use my_no_sql_grpc_core::schemas::EntitySchema;
use rust_extensions::date_time::DateTimeAsMicroseconds;
use tonic::transport::Endpoint;

use crate::app::AppContext;
use crate::my_no_sql_writer_grpc::*;

use super::DbOperationError;
use super::write::{mark_partitions_to_persist, mark_table_metadata_to_persist};

/// One chunk of a table on its way out: the rows, and the schema they were
/// written under.
///
/// Grouped by schema rather than by partition, because the schema is what has to
/// travel with them - a table whose rows were written under several entity
/// versions leaves as several chunks.
pub struct RowsWithSchema {
    pub schema: Option<Arc<EntitySchema>>,
    pub rows: Vec<Arc<DbRow>>,
}

/// Everything a table holds, cut the way it has to leave.
///
/// A row whose schema this server no longer has still goes: it is a row, and
/// losing it because its schema went missing would be worse than moving it
/// unshowable.
pub fn get_rows_with_schema(db_table: &DbTable) -> Vec<RowsWithSchema> {
    let mut by_schema: AHashMap<u64, Vec<Arc<DbRow>>> = AHashMap::new();

    for db_row in db_table.get_rows(&GetRowsFilter::all()) {
        by_schema
            .entry(db_row.get_schema_id())
            .or_default()
            .push(db_row);
    }

    by_schema
        .into_iter()
        .map(|(schema_id, rows)| RowsWithSchema {
            schema: db_table.get_schema(schema_id),
            rows,
        })
        .collect()
}

/// Pulls a table from another server of this kind into this one.
///
/// The schema travels with the rows, which is this server's rule everywhere
/// else: the destination puts a schema it has never seen into the table it is
/// filling and every row keeps the reference to it, so a migrated table is
/// showable on the other side without anybody copying a schema by hand.
///
/// The destination does the pulling, so it is the one which needs no inbound
/// route to the source - and the url it dials comes from the caller, which is
/// worth knowing when the caller is not an operator.
pub async fn migrate_from(
    app: &AppContext,
    request: MigrateFromGrpcRequest,
    persist_moment: DateTimeAsMicroseconds,
) -> Result<usize, DbOperationError> {
    let table_name = if request.table_name.is_empty() {
        request.remote_table_name.clone()
    } else {
        request.table_name.clone()
    };

    let mut client = connect(&request.remote_url).await?;

    let attributes = get_remote_attributes(
        &mut client,
        &request.remote_name_space,
        &request.remote_table_name,
    )
    .await?;

    let db_namespace = app
        .namespaces
        .get_or_create(&request.name_space, &app.settings)
        .await?;

    let mut stream = client
        .get_rows_with_schema(GetRowsWithSchemaGrpcRequest {
            name_space: request.remote_name_space,
            table_name: request.remote_table_name,
        })
        .await
        .map_err(migration_failed)?
        .into_inner();

    // Collected whole and applied in one entry into the table: a migration which
    // landed chunk by chunk would let a reader take a snapshot of half a table,
    // and the chunks are cut by schema, so each of them alone is not a state the
    // table was ever in.
    let mut db_rows = Vec::new();

    // The schemas arrive with the chunks and not with the attributes: what the
    // other server answers `GetTables` with is what a table is *set to*, and the
    // shapes its rows were written under are the data's, so they travel with the
    // data. They are held until the table exists, because the table is where
    // they now live.
    let mut schemas = Vec::new();

    while let Some(chunk) = stream.message().await.map_err(migration_failed)? {
        let schema_id = match chunk.schema {
            Some(schema) => {
                let schema: EntitySchema = schema.into();
                let schema_id = schema.id;

                schemas.push(schema);

                schema_id
            }
            // The source had no schema for these either. They still move.
            None => 0,
        };

        for row in chunk.rows {
            db_rows.push(to_db_row(&row, schema_id)?);
        }
    }

    let db_table = db_namespace
        .tables
        .get_or_create(&table_name, || {
            Arc::new(DbTable::new(table_name.clone(), attributes.clone()))
        })
        .table;

    db_table.set_attributes(attributes);

    for schema in schemas {
        db_table.register_schema(schema);
    }

    // Queued after the new attributes and the schemas are in place and
    // regardless of what they say: the migration which turned persistence off is
    // the one that most needs `tables.meta` rewritten, and the partitions below
    // need their old slots freed for the same reason.
    mark_table_metadata_to_persist(&db_namespace, &db_table, persist_moment);

    let migrated = db_rows.len();

    let result = db_table.bulk_write(BulkWriteMode::CleanTableAndInsert, db_rows);

    mark_partitions_to_persist(
        &db_namespace,
        &db_table,
        &result.partitions_to_persist,
        persist_moment,
    );

    crate::db_operations::sync::bulk_written(
        app,
        &db_namespace,
        &db_table.name,
        BulkWriteMode::CleanTableAndInsert,
        result.written,
    );

    Ok(migrated)
}

async fn connect(
    url: &str,
) -> Result<writer_client::WriterClient<tonic::transport::Channel>, DbOperationError> {
    let endpoint = Endpoint::from_shared(url.to_string()).map_err(|_| {
        DbOperationError::MigrationFailed(format!(
            "'{url}' is not a url a gRPC endpoint can be built from"
        ))
    })?;

    let channel = endpoint
        .connect()
        .await
        .map_err(|err| DbOperationError::MigrationFailed(format!("can not reach {url}: {err}")))?;

    Ok(writer_client::WriterClient::new(channel))
}

/// The table arrives with the attributes it had there: a migration which reset
/// them to the defaults would quietly turn off persistence or a limit.
async fn get_remote_attributes(
    client: &mut writer_client::WriterClient<tonic::transport::Channel>,
    name_space: &str,
    table_name: &str,
) -> Result<my_no_sql_grpc_core::db::DbTableAttributes, DbOperationError> {
    let mut stream = client
        .get_tables(GetTablesGrpcRequest {
            name_space: name_space.to_string(),
        })
        .await
        .map_err(migration_failed)?
        .into_inner();

    while let Some(table) = stream.message().await.map_err(migration_failed)? {
        if table.name == table_name {
            return Ok(crate::grpc_server::mappers::to_db_table_attributes(
                table.attributes,
            ));
        }
    }

    Err(DbOperationError::MigrationFailed(format!(
        "the other server has no table '{table_name}' in namespace '{name_space}'"
    )))
}

fn migration_failed(err: tonic::Status) -> DbOperationError {
    DbOperationError::MigrationFailed(format!(
        "the other server answered {}: {}",
        err.code(),
        err.message()
    ))
}

fn to_db_row(row: &[u8], schema_id: u64) -> Result<Arc<DbRow>, DbOperationError> {
    let parsed = ParsedEntity::parse(row)?;

    // A row always leaves a server in its emit form, so it carries its TimeStamp
    // - and a migration keeps it rather than stamping this server's clock: the
    // row is the same row it was there.
    let time_stamp = match parsed.time_stamp {
        Some(time_stamp) => DateTimeAsMicroseconds::new(time_stamp),
        None => DateTimeAsMicroseconds::now(),
    };

    Ok(Arc::new(DbRow::new(parsed, schema_id, time_stamp)))
}
