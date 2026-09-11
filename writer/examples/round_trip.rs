//! Writes through the writer and watches the same rows arrive in the reader,
//! against a server running in another process.
//!
//! ```text
//! cargo run --example round_trip -- http://127.0.0.1:5124
//! ```
//!
//! The tests already drive both clients over a socket, but always against a
//! server living in the same process. This is the same round trip against a
//! server that was started on its own, which is the only way to find out that
//! the two sides really are two sides.

use std::time::Duration;

use my_no_sql_grpc_macros::my_no_sql_entity;
use my_no_sql_grpc_reader::MyNoSqlGrpcReader;
use my_no_sql_grpc_writer::{
    MyNoSqlGrpcConnection, MyNoSqlGrpcWriter, SyncPeriodGrpcModel, TableAttributesGrpcModel,
};

#[my_no_sql_entity(table_name: "example-traders")]
#[derive(Clone, Debug)]
pub struct TraderEntity {
    #[proto_no(5)]
    pub amount: f64,
}

fn trader(partition_key: &str, row_key: &str, amount: f64) -> TraderEntity {
    TraderEntity {
        partition_key: partition_key.to_string(),
        row_key: row_key.to_string(),
        amount,
        ..Default::default()
    }
}

#[tokio::main]
async fn main() {
    let url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "http://127.0.0.1:5124".to_string());

    println!("Talking to {url}");

    let writer: MyNoSqlGrpcWriter<TraderEntity> =
        MyNoSqlGrpcWriter::new(MyNoSqlGrpcConnection::new(url.clone()).unwrap())
            .with_sync_period(SyncPeriodGrpcModel::SyncPeriodImmediately);

    writer
        .create_table_if_not_exists(TableAttributesGrpcModel {
            persist: true,
            max_partitions_amount: None,
            max_rows_per_partition_amount: None,
        })
        .await
        .unwrap();

    writer.clean_table().await.unwrap();
    writer
        .insert_or_replace(&trader("acc-1", "before", 1.5))
        .await
        .unwrap();

    let reader = MyNoSqlGrpcReader::new(url, "round-trip-example", "1.0.0").unwrap();
    let traders = reader.subscribe::<TraderEntity>();
    reader.start();

    traders.wait_until_initialized().await;
    println!("snapshot: {} row(s)", traders.get_rows_amount());

    let batch: Vec<TraderEntity> = (0..2_000)
        .map(|no| trader(&format!("acc-{}", no % 4), &format!("rk-{no}"), no as f64))
        .collect();
    writer.bulk_insert_or_replace(&batch).await.unwrap();

    let mut transaction = writer.begin_transaction().await.unwrap();
    transaction.delete_partitions(vec!["acc-0".to_string()]);
    transaction.insert_or_replace(&[trader("acc-9", "from-transaction", 42.0)]);
    transaction.commit().await.unwrap();

    for _ in 0..200 {
        if traders
            .get_row("acc-9", "from-transaction")
            .unwrap()
            .is_some()
        {
            break;
        }

        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    println!("after the batch and the transaction:");
    println!("  rows in the reader: {}", traders.get_rows_amount());
    println!("  partitions: {:?}", traders.get_partition_keys());
    println!(
        "  the transaction's row: {:?}",
        traders
            .get_row("acc-9", "from-transaction")
            .unwrap()
            .map(|itm| itm.amount)
    );
    println!(
        "  the cleaned partition: {} row(s)",
        traders.get_by_partition_key("acc-0").unwrap().len()
    );
    println!(
        "  the writer agrees: {} row(s)",
        writer.get_rows(None, None).await.unwrap().len()
    );

    reader.stop();
}
