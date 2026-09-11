use crate::app::AppContext;

/// Writes everything still queued, ignoring due moments: a change which was
/// asked to wait a minute must not be lost because the process stopped in the
/// meantime.
pub async fn shutdown(app: &AppContext) {
    println!("Shutting down. Draining the persist queue...");

    let mut written = 0;

    while crate::operations::persist(app, None).await {
        written += 1;
    }

    // The drain runs with no due-moment filter, so an item left behind would
    // mean the queue grew while we were emptying it - worth saying out loud
    // rather than losing quietly.
    for db_namespace in app.namespaces.get_all() {
        if db_namespace.persist_markers.has_something_to_persist() {
            my_logger::LOGGER.write_error(
                "shutdown",
                format!(
                    "Namespace '{}' still has queued changes after the drain",
                    db_namespace.name
                ),
                my_logger::LogEventCtx::new(),
            );
        }
    }

    println!("Persist queue drained: {written} task(s) written. Shutdown is complete.");
}
