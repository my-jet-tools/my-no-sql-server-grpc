use std::sync::Arc;
use std::time::Duration;

use app::AppContext;
use background::{
    BackupTimer, GcTimer, PersistTimer, ReaderSessionsGcTimer, TransactionsGcTimer, VacuumTimer,
};
use rust_extensions::MyTimer;

mod app;
mod background;
mod consts;
mod data_sync_period;
mod db_operations;
#[cfg(test)]
mod end_to_end_tests;
mod grpc_server;
mod http_server;
mod json_view;
mod listen_endpoints;
mod mcp;
mod monitoring;
mod operations;
mod persist;
mod reader;
#[cfg(test)]
mod sdk_tests;
mod settings_reader;
mod transactions;

pub mod my_no_sql_writer_grpc {
    tonic::include_proto!("my_no_sql_writer");
}

pub mod my_no_sql_reader_grpc {
    tonic::include_proto!("my_no_sql_reader");
}

#[tokio::main]
async fn main() {
    let settings = Arc::new(settings_reader::read_settings().await);

    let app = Arc::new(AppContext::new(settings));

    println!(
        "{} v{} is starting. Persistence dest: {}",
        app::APP_NAME,
        app::APP_VERSION,
        app.settings.get_persistence_dest()
    );

    // Requests are refused until this finishes - a table served half-loaded looks
    // exactly like a table that lost its data.
    crate::operations::load_from_disk(app.clone()).await;

    let mut persist_timer = MyTimer::new(Duration::from_secs(1));
    persist_timer.register_timer("Persist", Arc::new(PersistTimer::new(app.clone())));
    persist_timer.start(app.states.clone(), my_logger::LOGGER.clone());

    let mut timer_10s = MyTimer::new(Duration::from_secs(10));
    timer_10s.register_timer(
        "GcReaderSessions",
        Arc::new(ReaderSessionsGcTimer::new(app.clone())),
    );
    timer_10s.register_timer(
        "GcTransactions",
        Arc::new(TransactionsGcTimer::new(app.clone())),
    );
    timer_10s.start(app.states.clone(), my_logger::LOGGER.clone());

    let mut timer_30s = MyTimer::new(Duration::from_secs(30));
    timer_30s.register_timer("Gc", Arc::new(GcTimer::new(app.clone())));
    timer_30s.start(app.states.clone(), my_logger::LOGGER.clone());

    let mut vacuum_timer = MyTimer::new(Duration::from_secs(60));
    vacuum_timer.register_timer("Vacuum", Arc::new(VacuumTimer::new(app.clone())));
    vacuum_timer.start(app.states.clone(), my_logger::LOGGER.clone());

    // Only when the operator asked for it: where the backups go and how often
    // are both settings, and a server which invented them would be writing
    // gigabytes nobody asked for.
    if let Some(interval_secs) = app.settings.backup_interval_secs
        && app.backups.is_configured()
    {
        let mut backup_timer = MyTimer::new(Duration::from_secs(interval_secs.min(60)));
        backup_timer.register_timer(
            "Backup",
            Arc::new(BackupTimer::new(app.clone(), interval_secs)),
        );
        backup_timer.start(app.states.clone(), my_logger::LOGGER.clone());
    }

    crate::http_server::start_up::start(&app, app.http_endpoint);

    tokio::spawn(crate::grpc_server::start(app.clone(), app.grpc_endpoint));

    app.states.wait_until_shutdown().await;

    crate::operations::shutdown(&app).await;
}
