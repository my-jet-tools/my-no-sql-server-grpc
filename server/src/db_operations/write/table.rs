use std::sync::Arc;

use my_no_sql_grpc_core::db::{DbTable, DbTableAttributes};
use rust_extensions::date_time::DateTimeAsMicroseconds;

use crate::app::{AppContext, DbNamespace};
use crate::db_operations::DbOperationError;

use super::{mark_partitions_to_persist, mark_table_metadata_to_persist};

pub fn create_table(
    db_namespace: &DbNamespace,
    table_name: &str,
    attributes: DbTableAttributes,
    persist_moment: DateTimeAsMicroseconds,
) -> Result<Arc<DbTable>, DbOperationError> {
    let result = create(db_namespace, table_name, attributes, persist_moment);

    if !result.created {
        return Err(DbOperationError::TableAlreadyExists(table_name.to_string()));
    }

    Ok(result.table)
}

/// Brings the table into existence if it is not there - and applies the
/// attributes either way.
///
/// Applying them to a table which was already there is the whole point of the
/// call every service makes at start up with the attributes it wants: a service
/// which raised its row limit and redeployed would otherwise go on evicting at
/// the old one, with nothing anywhere to say why. `created` survives it, because
/// setting a limit is not founding a table.
pub fn create_table_if_not_exists(
    app: &AppContext,
    db_namespace: &DbNamespace,
    table_name: &str,
    attributes: DbTableAttributes,
    persist_moment: DateTimeAsMicroseconds,
) -> Arc<DbTable> {
    let result = create(db_namespace, table_name, attributes.clone(), persist_moment);

    // A start-up call which changes nothing must stay free: this is the call
    // every service repeats on every deploy, and queueing a `tables.meta` write
    // and waking every subscriber for attributes that already say the same thing
    // would be a cost paid by everyone to help nobody.
    if !result.created && says_something_new(&result.table.get_attributes(), &attributes) {
        set_table_attributes(app, db_namespace, &result.table, attributes, persist_moment);
    }

    result.table
}

/// `created` is left out on purpose - it is the table's own history, not
/// something the caller can ask for.
fn says_something_new(current: &DbTableAttributes, incoming: &DbTableAttributes) -> bool {
    current.persist != incoming.persist
        || current.max_partitions_amount != incoming.max_partitions_amount
        || current.max_rows_per_partition_amount != incoming.max_rows_per_partition_amount
}

/// Empties the table, keeping the table itself.
///
/// The instruction reaches the subscribers even when there was nothing to empty:
/// unlike a list of partitions, "clean the table" has no payload which could be
/// empty, so there is no such thing as a clean that does not apply.
pub fn clean_table(
    app: &AppContext,
    db_namespace: &DbNamespace,
    db_table: &DbTable,
    persist_moment: DateTimeAsMicroseconds,
) {
    let partition_keys = db_table.clean();

    mark_partitions_to_persist(db_namespace, db_table, &partition_keys, persist_moment);

    crate::db_operations::sync::table_cleaned(app, db_namespace, &db_table.name);
}

/// Changes what a table does with itself - its limits, and whether it is
/// persisted at all.
///
/// `created` is carried over rather than taken from the request: it is when the
/// table came into being, and setting a limit is not a new table.
pub fn set_table_attributes(
    app: &AppContext,
    db_namespace: &DbNamespace,
    db_table: &DbTable,
    mut attributes: DbTableAttributes,
    persist_moment: DateTimeAsMicroseconds,
) {
    attributes.created = db_table.get_attributes().created;

    db_table.set_attributes(attributes);

    // The metadata is written whenever the table is persisted now - including
    // the call which just turned persistence off, because `tables.meta` has to
    // stop saying it is on.
    db_namespace
        .persist_markers
        .persist_table_metadata(&db_table.name, persist_moment);

    crate::db_operations::sync::table_attributes_updated(
        app,
        db_namespace,
        &db_table.name,
        db_table.get_attributes(),
    );
}

/// Removes the table itself.
///
/// The table leaves the namespace first: from that moment nothing can resolve it
/// by name, so no write can land in what is about to be emptied. Emptying it
/// afterwards is what hands over the partition keys whose slots have to be
/// freed - the persist loop looks each of them up, finds no table, and deletes
/// it, and does the same with the table's entry in `tables.meta`.
///
/// A crash between those two writes leaves slots whose table is not in
/// `tables.meta` any more, and the next start restores them into a table with
/// default attributes. That is the same class of loss as a crash right after the
/// call - the delete simply did not happen - and the load path already says so
/// out loud.
pub fn delete_table(
    app: &AppContext,
    db_namespace: &DbNamespace,
    table_name: &str,
    persist_moment: DateTimeAsMicroseconds,
) -> Result<(), DbOperationError> {
    let Some(db_table) = db_namespace.tables.remove(table_name) else {
        return Err(DbOperationError::TableNotFound(table_name.to_string()));
    };

    let partition_keys = db_table.clean();

    mark_partitions_to_persist(db_namespace, &db_table, &partition_keys, persist_moment);
    mark_table_metadata_to_persist(db_namespace, &db_table, persist_moment);

    crate::db_operations::sync::table_deleted(app, db_namespace, table_name);

    Ok(())
}

fn create(
    db_namespace: &DbNamespace,
    table_name: &str,
    attributes: DbTableAttributes,
    persist_moment: DateTimeAsMicroseconds,
) -> my_no_sql_grpc_core::db::GetOrCreateTableResult {
    let result = db_namespace.tables.get_or_create(table_name, || {
        Arc::new(DbTable::new(table_name.to_string(), attributes))
    });

    // Only a table that was actually created needs its metadata written here; a
    // repeated CreateTableIfNotExists queues it through set_table_attributes
    // instead, and only when there is something new to say.
    //
    // Whether the table is persisted or not does not come into it: `tables.meta`
    // is the record of which tables there are and what they are set to, and a
    // table missing from it is a table the next start does not bring back.
    if result.created {
        db_namespace
            .persist_markers
            .persist_table_metadata(table_name, persist_moment);
    }

    result
}
