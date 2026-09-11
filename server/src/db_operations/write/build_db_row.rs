use std::sync::Arc;

use my_no_sql_grpc_core::db::{DbRow, DbTable};
use my_no_sql_grpc_core::db_entity::{DbEntityParseFail, ParsedEntity};
use my_no_sql_grpc_core::schemas::EntitySchema;
use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::app::DbNamespace;
use crate::db_operations::{DbOperationError, WriteRowRequest};

/// A row ready to be written, together with the moment its partition has to
/// reach the disk by.
pub struct PreparedRow {
    pub db_row: Arc<DbRow>,
    pub persist_moment: DateTimeAsMicroseconds,
}

/// Turns what arrived over the wire into a row of the database: the entity is
/// taken apart, its schema is remembered if the server has not seen it before,
/// and `TimeStamp` is decided.
pub fn build_db_row(
    db_namespace: &DbNamespace,
    db_table: &DbTable,
    request: WriteRowRequest,
    now: DateTimeAsMicroseconds,
) -> Result<PreparedRow, DbOperationError> {
    let persist_moment = request.sync_period.get_sync_moment(now);

    let parsed = ParsedEntity::parse(&request.row)?;
    let schema_id = register_schema(db_namespace, db_table, request.schema, now)?;

    Ok(PreparedRow {
        db_row: build_row(parsed, schema_id, request.use_client_time_stamp, now)?,
        persist_moment,
    })
}

/// The same thing for a batch. The schema is remembered once instead of once per
/// entity - it travels with every message of a bulk write, and it is the largest
/// thing on that wire.
pub fn build_db_rows(
    db_namespace: &DbNamespace,
    db_table: &DbTable,
    schema: EntitySchema,
    rows: &[Vec<u8>],
    use_client_time_stamp: bool,
    now: DateTimeAsMicroseconds,
) -> Result<Vec<Arc<DbRow>>, DbOperationError> {
    let schema_id = register_schema(db_namespace, db_table, schema, now)?;

    let mut result = Vec::with_capacity(rows.len());

    for row in rows {
        let parsed = ParsedEntity::parse(row)?;
        result.push(build_row(parsed, schema_id, use_client_time_stamp, now)?);
    }

    Ok(result)
}

/// The schema comes along with every entity, so an id the table has already seen
/// means there is nothing to store - and that is the normal case. A genuinely
/// new one goes into the table's own attributes, and the queue is told so at the
/// moment the row itself asked for: the metadata of a table is always handed out
/// before its partitions, so the shape can not reach the disk later than the
/// rows that need it.
///
/// What the known case is *not* is a case with nothing to do: the bytes are
/// compared. The id is a constant the client folded out of its own type and this
/// server never recomputes - one hash, on one side, with nothing to drift - and
/// the price of not recomputing is that nothing about the wire stops a second
/// client, written from the published proto, from putting its own number there.
/// One such client would make this server render one table's rows through
/// another table's field names, silently and for as long as those rows live,
/// because the id goes inside them. The schema is on that wire anyway, so
/// comparing it costs less than receiving it.
fn register_schema(
    db_namespace: &DbNamespace,
    db_table: &DbTable,
    schema: EntitySchema,
    now: DateTimeAsMicroseconds,
) -> Result<u64, DbOperationError> {
    let schema_id = schema.id;

    if let Some(already) = db_table.get_schema(schema_id) {
        check_it_is_the_same_schema(&already, &schema)?;
        return Ok(schema_id);
    }

    if db_table.register_schema(schema) {
        super::mark_table_metadata_to_persist(db_namespace, db_table, now);
    }

    Ok(schema_id)
}

fn check_it_is_the_same_schema(
    already: &EntitySchema,
    arriving: &EntitySchema,
) -> Result<(), DbEntityParseFail> {
    if already.schema == arriving.schema {
        return Ok(());
    }

    Err(DbEntityParseFail::SchemaIdIsAlreadyTakenByAnotherSchema {
        schema_id: arriving.id,
    })
}

fn build_row(
    parsed: ParsedEntity,
    schema_id: u64,
    use_client_time_stamp: bool,
    now: DateTimeAsMicroseconds,
) -> Result<Arc<DbRow>, DbOperationError> {
    let time_stamp = if use_client_time_stamp {
        match parsed.time_stamp {
            Some(time_stamp) => DateTimeAsMicroseconds::new(time_stamp),
            None => {
                return Err(DbEntityParseFail::TimeStampIsRequired {
                    partition_key: parsed.get_partition_key().to_string(),
                    row_key: parsed.get_row_key().to_string(),
                }
                .into());
            }
        }
    } else {
        now
    };

    Ok(Arc::new(DbRow::new(parsed, schema_id, time_stamp)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same id arriving with the same bytes is the normal case: the schema
    /// travels with every single entity, so this happens on every write after the
    /// first.
    #[test]
    fn the_same_schema_under_the_same_id_is_taken() {
        let schema = EntitySchema::new(777, vec![10, 20, 30]);

        assert!(check_it_is_the_same_schema(&schema.clone(), &schema).is_ok());
    }

    /// A second client, written from the published proto, which fills SchemaId
    /// with a constant of its own: two shapes then share an id, and whichever
    /// arrived first is the one every row of both is shown through - silently,
    /// and for as long as the rows live.
    #[test]
    fn a_second_shape_under_a_known_id_is_refused() {
        let already = EntitySchema::new(777, vec![10, 20, 30]);
        let arriving = EntitySchema::new(777, vec![10, 20, 31]);

        assert!(matches!(
            check_it_is_the_same_schema(&already, &arriving),
            Err(DbEntityParseFail::SchemaIdIsAlreadyTakenByAnotherSchema { schema_id: 777 })
        ));
    }
}
