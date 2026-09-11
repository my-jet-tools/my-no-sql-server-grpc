# my-no-sql-server-grpc

MyNoSqlServer whose entities are **protobuf rather than JSON**. A table is held
in memory in full, the key is composite - `PartitionKey` + `RowKey` - changes are
pushed to subscribers, and a subscriber keeps the same table on its side and
answers from it without leaving the process.

The JSON version (`MyNoSqlServer`) is the reference for what this one does; what
is done differently here, and why, is written down in [DESIGN.md](DESIGN.md) -
worth reading before changing anything.

## What it does

- **Writes**: `Insert`, `InsertOrReplace`, `InsertOrReplaceIfNew`, `Replace`,
  `DeleteRow`, `BulkDelete`, `BulkWrite` in four modes, `CleanTable`,
  `DeletePartitions`, `DeleteTable`.
- **Reads**: one row, a partition, the whole table (streamed in bounded chunks),
  `HighestRowAndBelow`, `SinglePartitionMultipleRows`, table size, table list.
- **Transactions**: `Start` / `Post` / `Commit` / `Cancel` - what was accumulated
  is applied in one piece and reaches subscribers in one piece.
- **Subscription**: a snapshot of the table first, then instructions about what
  changed; the session has no delivery acknowledgements - a client that failed
  simply subscribes again.
- **Persistence**: to disk, with a sync period configurable per table; loaded at
  start up, and until that finishes requests are refused.
- **GC**: `Expires` on rows, eviction by the partition and row limits, and
  sliding expiration where reading is what keeps a row alive.
- **Namespaces**: tables are laid out by namespace, and a table can be moved
  between them.
- **Backups**: a zip per namespace - take one, inspect it, download it, upload
  it, restore the whole thing or a single partition; on a timer if the operator
  configured one.
- **HTTP** (for the UI and the operator): reads rendered by the table's schema,
  writes without an entity, status, connections, statistics, Prometheus
  `/metrics`, swagger UI.
- **MCP** at `/mcp`: 15 tools and 2 prompts. Its writes are shut until a human
  calls `POST /api/Mcp/Writes?enabled=true` - a window of 10 minutes.

The entity's schema travels **along with the write**: the server does not need it
to find the keys - their numbers are fixed by the contract - it needs it to show
a stored row to a human under the row's own field names.

## Ports

| Port | What is there |
|------|---------------|
| 5124 | gRPC - `Writer` and `Reader`, the entrance applications use |
| 5123 | HTTP - swagger UI, `/metrics`, MCP at `/mcp`, the operator's handles |

## Running it

Settings are YAML in `~/.mynosqlservergrpc`:

```yaml
PersistenceDest: ~/mynosql-data      # where to persist; every folder inside is a namespace
Location: dev-01                     # instance label, visible in /metrics and in the status
CompressData: false
SkipBrokenPartitions: false
BackupsDest: ~/mynosql-backups       # optional; without it backups are not configured
BackupIntervalSecs: 3600             # optional; without it, only by hand
MaxBackups: 24                       # optional; without it, all of them are kept
ApiKey: secret                       # optional; without it HTTP is open
```

`ApiKey` is what the HTTP surface asks for, in the `apikey` header; `/api/IsAlive`
and the swagger UI stay open - a browser has no way to attach a header. gRPC
deliberately has no key: it is the write transport and it is not meant to be
exposed.

```bash
cargo run -p my-no-sql-server-grpc
# or the image the release workflow builds:
docker run -p 5123:5123 -p 5124:5124 \
  -v ~/.mynosqlservergrpc:/root/.mynosqlservergrpc \
  -v ~/mynosql-data:/mynosql-data \
  ghcr.io/my-jet-tools/my-no-sql-server-grpc:<tag>
```

The settings file is mounted in: its path is hard-wired as `~/.mynosqlservergrpc`,
and the `PersistenceDest` inside it has to point at the mounted volume.

## Layout

| Folder | What is in it |
|--------|---------------|
| [server/](server/) | the server: gRPC, persistence, HTTP, MCP, GC, backups |
| [proto/](proto/) | the contracts themselves, compiled in place by the build script |

The core, the entity macro and both clients live in
[my-no-sql-grpc-sdk](https://github.com/my-jet-tools/my-no-sql-grpc-sdk), which
this repository depends on by tag - the very same crates an application takes
from there. The SDK keeps a committed copy of `proto/`, and a CI job of its own
tells it when the copy went stale.

## Clients

An application takes one dependency - the SDK facade - and turns on what it
needs:

```toml
[dependencies]
my-no-sql-grpc-sdk = { tag = "0.1.0", git = "https://github.com/my-jet-tools/my-no-sql-grpc-sdk.git", features = [
    "macros",
    "data-writer",
    "data-reader",
] }
```

```rust
use my_no_sql_grpc_sdk::macros::my_no_sql_entity;
use my_no_sql_grpc_sdk::reader::MyNoSqlGrpcReader;
use my_no_sql_grpc_sdk::writer::{MyNoSqlGrpcConnection, MyNoSqlGrpcWriter};

#[my_no_sql_entity(table_name: "traders")]
#[derive(Clone, Debug)]
pub struct TraderEntity {
    #[proto_no(5)]
    pub amount: f64,
}

async fn use_it(url: &str) {
    let writer: MyNoSqlGrpcWriter<TraderEntity> =
        MyNoSqlGrpcWriter::new(MyNoSqlGrpcConnection::new(url).unwrap());
    writer
        .insert_or_replace(&TraderEntity {
            partition_key: "acc-1".to_string(),
            row_key: "eur-usd".to_string(),
            amount: 1.5,
            ..Default::default()
        })
        .await
        .unwrap();

    let reader = MyNoSqlGrpcReader::new(url, "my-app", "1.0.0").unwrap();
    let traders = reader.subscribe::<TraderEntity>();
    reader.start();
    traders.wait_until_initialized().await;

    println!("{:?}", traders.get_row("acc-1", "eur-usd").unwrap());
}
```

The entity contract - which field numbers are reserved, what types a field may
have, what `TimeStamp` and `Expires` mean - and the whole of both clients are
documented in the
[SDK's README](https://github.com/my-jet-tools/my-no-sql-grpc-sdk#readme). What
matters on this side: the schema travels along with every write, the four
reserved fields are the server's contract rather than protobuf's, and the server
never needs the schema to find the keys.

## Development

```bash
cargo test                                    # 210 tests, 31 of them over a real socket
cargo clippy --all-targets -- -D warnings
cargo fmt --all

# both clients against a server in another process - the example lives in the SDK
cargo run -p my-no-sql-server-grpc
cd ../my-no-sql-grpc-sdk && cargo run --example round_trip --all-features -- http://127.0.0.1:5124
```

The contracts are exercised on two levels: [server/src/sdk_tests.rs](server/src/sdk_tests.rs)
starts a real server on a socket and drives both clients - the ones resolved from
the SDK tag, so the tests fail if a released client stops matching this server -
and the SDK's `round_trip` example does the same against a server in its own
process. The build broke in exactly the place where the tests were passing, which
is why both levels exist.
