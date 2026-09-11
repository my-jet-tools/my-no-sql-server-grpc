use std::net::SocketAddr;
use std::sync::Arc;

use crate::app::AppContext;
use crate::my_no_sql_reader_grpc::reader_server::ReaderServer;
use crate::my_no_sql_writer_grpc::writer_server::WriterServer;

use super::{ReaderGrpcService, WriterGrpcService};

/// Every service of this server shares one port.
pub async fn start(app: Arc<AppContext>, port: u16) {
    let addr = SocketAddr::from(([0, 0, 0, 0], port));

    println!("Listening gRPC at: {addr}");

    let writer = WriterServer::new(WriterGrpcService::new(app.clone()));
    let reader = ReaderServer::new(ReaderGrpcService::new(app));

    tonic::transport::Server::builder()
        .add_service(writer)
        .add_service(reader)
        .serve(addr)
        .await
        .unwrap();
}
