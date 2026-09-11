//! What every tool of this surface needs before it can do anything: the
//! namespace, the table, the schema a row is written through, and permission to
//! write at all.
//!
//! It is one module rather than a helper per tool because these are the four
//! answers a caller has to be able to act on, and a tool which phrases one of
//! them differently is a tool the model learns a different lesson from.

use std::sync::Arc;

use my_no_sql_grpc_abstractions::schemas::EntitySchema;
use my_no_sql_grpc_core::db::DbTable;
use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::app::{AppContext, DbNamespace};
use crate::json_view::SchemaIndex;

/// Refuses while the server is still reading its tables off the disk.
///
/// The same bounce the gRPC surface makes, and for the same reason: a table
/// which is half loaded answers "no rows", and "no rows" is an answer a caller
/// acts on.
pub fn check_if_initialized(app: &AppContext) -> Result<(), String> {
    if app.states.is_initialized() {
        return Ok(());
    }

    Err("The server is still loading its tables from disk. Try again in a moment.".to_string())
}

/// The namespace a call names, **without** creating it.
///
/// Reading a namespace nobody has written to is a mistake worth hearing about,
/// and creating one as a side effect would leave a folder on disk behind every
/// typo. The single exception is the destination of a table move, which resolves
/// with [`get_or_create_namespace`] - bringing the named thing into existence is
/// that call's whole job.
pub fn get_namespace(
    app: &Arc<AppContext>,
    name: Option<&str>,
) -> Result<Arc<DbNamespace>, String> {
    check_if_initialized(app)?;

    let name = name.unwrap_or("");

    app.namespaces.get(name).ok_or_else(|| {
        format!(
            "There is no namespace '{}'. Call get_namespaces to see which there are.",
            crate::app::DbNamespaces::resolve_name(name)
        )
    })
}

pub async fn get_or_create_namespace(
    app: &Arc<AppContext>,
    name: Option<&str>,
) -> Result<Arc<DbNamespace>, String> {
    check_if_initialized(app)?;

    app.namespaces
        .get_or_create(name.unwrap_or(""), &app.settings)
        .await
        .map_err(|err| err.to_string())
}

pub fn get_table(db_namespace: &DbNamespace, table_name: &str) -> Result<Arc<DbTable>, String> {
    crate::db_operations::read::get_table(db_namespace, table_name).map_err(|err| {
        format!("{err}. Call get_list_of_tables to see which tables the namespace has.")
    })
}

/// The gate under every write tool.
///
/// It is shut by default and there is no key to hand the model: a person opens
/// it with `POST /api/Mcp/Writes?enabled=true`, which is not one of the tools
/// registered here and therefore not something the model can call. Ten minutes
/// later it shuts itself.
pub fn ensure_writes_are_open(app: &AppContext) -> Result<(), String> {
    if app.mcp_writes_are_open(DateTimeAsMicroseconds::now()) {
        return Ok(());
    }

    Err(
        "MCP write operations are currently SHUT. Ask the user to open them - \
         `POST /api/Mcp/Writes?enabled=true` on this server's HTTP port, which \
         opens them for 10 minutes - and wait for them to confirm. Do not retry \
         until they do. See prompt 'mcp_writes_enable_policy'."
            .to_string(),
    )
}

/// The schema a row handed to this surface as JSON is written through.
///
/// Nothing here registers a schema. A schema arrives with the entity on the
/// gRPC write path, under an id its client folded out of its own type, and the
/// table refuses a second shape under an id it already has - so a shape typed at
/// a keyboard has no business being the first thing a table learns. What this
/// surface can do is write **through a shape the table already knows**, which is
/// also what makes the rows it writes readable by everyone else.
pub fn pick_schema(
    db_table: &DbTable,
    schema_id: Option<u64>,
) -> Result<Arc<EntitySchema>, String> {
    let attributes = db_table.get_attributes();
    let schemas = &attributes.schemas;

    if let Some(schema_id) = schema_id {
        return schemas.get(&schema_id).cloned().ok_or_else(|| {
            format!(
                "Table '{}' has no schema {schema_id}. It has: {}",
                db_table.name,
                list_ids(schemas.keys().copied())
            )
        });
    }

    let mut ids = schemas.keys().copied();

    let Some(only) = ids.next() else {
        return Err(format!(
            "Table '{}' has never been written to, so this server does not know \
             the shape of its rows and can not turn JSON into one. The service \
             which owns this entity has to write a row first - the shape travels \
             with the write.",
            db_table.name
        ));
    };

    // More than one shape means a deploy is in flight, or two entities are
    // aimed at one table. Either way the caller has to say which - picking for
    // them would store a row under the version they did not mean, and the rows
    // outlive the guess.
    if ids.next().is_some() {
        return Err(format!(
            "Table '{}' holds rows of more than one entity version. Name the one \
             to write through in `schema_id`: {}",
            db_table.name,
            list_ids(schemas.keys().copied())
        ));
    }

    Ok(schemas.get(&only).cloned().unwrap())
}

fn list_ids(ids: impl Iterator<Item = u64>) -> String {
    let mut ids: Vec<u64> = ids.collect();
    ids.sort_unstable();

    if ids.is_empty() {
        return "none".to_string();
    }

    ids.iter()
        .map(|id| id.to_string())
        .collect::<Vec<String>>()
        .join(", ")
}

/// The resolved schema, ready to turn JSON into a row.
///
/// A schema which does not parse is the one case where the table knows a shape
/// and this surface still can not use it - the renderer shows such rows by field
/// number, but there is no field number to write *to*.
pub fn build_index(app: &AppContext, schema: &EntitySchema) -> Result<Arc<SchemaIndex>, String> {
    app.json_schemas.get_or_build(schema).ok_or_else(|| {
        format!(
            "Schema {} is stored but does not read back, so rows can not be built \
             through it.",
            schema.id
        )
    })
}
