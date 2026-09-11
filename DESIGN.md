# my-no-sql-server-grpc - decisions taken and what is left

MyNoSqlServer whose entities are protobuf, not JSON. By default `0.0.0.0:8888` -
gRPC (`Writer` + `Reader` on one port), `0.0.0.0:8000` - HTTP: reads and those
writes which do not carry an entity.

## Entity

```proto
string PartitionKey = 1;  string RowKey  = 2;
int64  TimeStamp    = 3;  int64  Expires = 4;   // unix µs, Expires 0 = never
// the user's own fields from 5 on
```

The server parses a row **without knowing the schema** - by the wire-format
tags. Fields 3 and 4 are **not stored** in the raw bytes: they are the server's
values and are appended when the row is handed out. Hence `raw` is immutable,
`update_expires` is an atomic, and the varint/fixed question never arises. An
empty PK/RK is indistinguishable from a missing one (proto3 does not write empty
strings) and is rejected.

**Both moments are checked on the way in.** `TimeStamp` and `Expires` are
rendered on every HTTP read, and the calendar they are rendered by ends at the
year 262142: a value past its edge is not shown but brings the read down with a
panic. The row is already saved by then, so it would keep falling over after
every restart too, and an `Expires` far in the future is on top of that never
collected by the GC. And this is not exotic: `0` already means "never", so
`i64::MAX` in the same sense is a natural mistake for an application to make.
The bounds live in the core as a separate function, not inside the parser: TTL
renewal out of read statistics is obliged to ask the same question - the moment
gets there from the reader in exactly the same way. The renderer on top of that
**does not panic**: a moment that is already in the row is shown as a number,
otherwise one such value makes the whole partition unreadable.

**A field number outside `1..=2^29-1` is rejected.** The tag is a varint, and
the number was taken out of it by a cast to `u32`: a row was indexed under a
number that no protobuf decoder would ever get out of those very same bytes.

## Schemas

The client sends a `SchemaId` **and the schema with every entity**; on a
familiar id the server **compares the bytes** against the stored ones, on an
unfamiliar one it puts the bytes next to the table's other attributes. Every row
remembers its own schema, so rows of different entity versions live side by
side. The schema is needed only to **show** a row (HTTP/JSON), not to store it.

**On a write the schema is not parsed at all** - neither on a familiar id nor on
an unfamiliar one. A row is parsed by the wire-format tags, it has no need of
the schema, and comparing bytes does not require understanding them. The schema
is parsed on the **first render**, lazily (see "The cache of parsed schemas") -
which means a blob that is not a schema is accepted by the write and makes it
all the way to disk. This is the same decision by which a schema that fails to
decode does not bring the start-up load down: the price of the mistake is rows
shown by field numbers, whereas a refusal on the write would cost the rows
themselves to somebody whose schema is perfectly fine. You hear about it on the
very first read - the renderer logs why it did not take the schema.

**The registry is the table's attributes, not a namespace.** `schema_id ->
Schema` lives in `DbTableAttributes` and goes to disk in `tables.meta` (YAML,
the schema bytes base64url). The argument is restart: the schema and the data
are restored by **one act**. The persistence queue always hands out a table's
`TableMetadata` before its `Partition`, and `persist()` and `flush()` are
serialised on `app.persist_lock` - which means a row cannot land on disk earlier
than the metadata naming its table. And since the schema is **inside** that
metadata, the state "the rows are on disk but the schema is not" stops being
expressible. There is no separate file for schemas (`schemas.bin` per namespace)
any more: the file about exactly this table already existed, and a second one
would be a second source of truth about the same thing. The degradation "we
start with an empty schema registry" disappears along with it: the state "the
table is there, the rows are there, and there is nothing to show them under" no
longer happens.

From the same place comes the fact that the registry is never moved anywhere by
hand. A schema is an attribute of the table, and attributes travel with the
table: `MoveTableToNamespace` copies them into the new table, a backup puts them
in `<table>/.metadata`, a restore pours them back in. `set_attributes` at the
same time **merges schemas rather than replacing them**: a request that names a
limit names no schema at all, and `PUT /api/Tables/Attributes`, which replaces
the attributes, would otherwise leave every stored row unshowable. The only way
to lose a schema is the GC (see "Schema GC").

A schema is **our own format, not a `FileDescriptorSet`**. What that gives and
what it costs is in "Schema" below.

## Writer

`SyncPeriod` in every write request. `Namespace` is required (an empty string =
`default`). There is no `ApiKey` in gRPC: gRPC is the write transport, it is not
exposed outwards, and a key on it would be a second thing to rotate, without a
second guarantee. The protection is on HTTP, see "HTTP".

Two write paths, and both are applied as **one entry under the write lock**:

- `BulkWrite` - a batch of one kind, with no server-side state, one RPC;
- **a transaction** - writes of different kinds together (clean and insert). The
  stream is preserved even so: the actions pour in as a stream, and the
  transaction makes them one step.

Alongside them are single-shot operations which rise to neither a stream nor a
transaction:

- **`InsertOrReplaceIfNew` on one row** - the same mode as in the batch, but as
  a call. It is the only one of the single-row writes that can **refuse**, and a
  refusal here is a normal outcome, not an error, so the response says `Written`
  instead of leaving the caller guessing. `UseClientTimeStamp` is forced on -
  the server's clock would make the incoming row always newer and the whole call
  an `InsertOrReplace`;
- **`BulkDelete`** - delete rows by key across several partitions at once. It
  goes down the transaction path because that is what it is - a list of deletes
  under one lock - but there is nothing to open: deleting is the whole of it
  here, and the client does not have to keep an id alive between two round
  trips. It answers how many rows **were** there, not how many were named: a key
  which is not there is not an error;
- **`DeleteRow`** answers with the same thing: `Deleted` - whether there was
  anything to delete, not an error. Otherwise deleting one key and deleting that
  same key inside `BulkDelete` disagree on one and the same input, and ported
  cleanup which loops over keys breaks off at the first already expired one.
  `RowNotFound` stays where the operation really is a lookup. On HTTP this is a
  200, as in the JSON version.

### Replace

**Optimistic locking, as in the JSON version.** The `TimeStamp` of the incoming
entity is **the version at which the client read the row**, and the write is
rejected if the stored row is no longer that one: `Aborted` on gRPC. On HTTP
this response never happens - `Replace` carries an entity, and operations like
that are not let in there (see "HTTP"); the 409 in the shared error map is what
it would translate into, not a route which hands it out. Without this check both
of two processes editing the same row are told `Ok`, and one of the edits
vanishes without telling anybody about it. The comparison is under the same
write lock as the replacement itself (`DbTable::replace_if_version_matches`):
two entries into the table would let exactly the writer the check exists for
slip in between reading the version and the write.

- **the version and the new stamp are different moments**, and they must not be
  confused: the row which lands gets the server's clock, whatever
  `UseClientTimeStamp` says. Were it to keep the version it was built at, the
  next writer with that same read would pass the check too - that is, the check
  would be passed twice on one version. That is also why the loop "read →
  modified → wrote → on `Aborted` read again" terminates: every write which
  lands leaves behind a version nobody holds;
- **an entity without a `TimeStamp` is rejected** (`invalid_argument`): there is
  no version in it, there is nothing to check, and an `Ok` would mean the row
  was replaced under a check nobody performed;
- the row is not there at all - still `NotFound`: that is a different answer,
  and there is no point retrying it, unlike a conflict. The client tells them
  apart with `is_not_found()`/`is_conflict()`.

### BulkWrite

The stream is transport and nothing more: we accumulate all the entities, apply
them in one entry under the write lock, and put them into the subscribers' queue
as one event. Everything else follows from that:

- the header (namespace, table, mode, sync period, `UseClientTimeStamp`) is
  repeated in **every** message and has to match everywhere. We reject anything
  that disagrees - that costs nothing, because nothing has been written until
  the stream ends;
- **the schema is its own per message**: one batch can carry entities of
  different versions, and every row remembers the one it came with;
- an unknown mode is an error, not a default (unlike the sync period): the modes
  differ in **what they destroy**, and there is nothing to guess at here;
- an empty stream is an error (it does not even name the table), a batch with no
  rows is not: `CleanTableAndInsert` with no rows is exactly `CleanTable`;
- `InsertOrReplaceIfNew` compares the client's `TimeStamp` against the stored
  one, so `UseClientTimeStamp` is forced on in this mode. A row which lost the
  comparison is **not handed to the reader** - otherwise its cache would roll
  back in time.

### Transaction

`StartTransaction → id`, `PostTransactionActions` (a stream),
`CommitTransaction`, `CancelTransaction`. The transaction registry is built like
the reader sessions: an `ArcSwap` map, a 60 s TTL and a GC timer.

- **one table per transaction**, named once in `StartTransaction`. The actions
  carry only the `TransactionId` and the keys, the table is resolved through the
  transaction. Cross-table atomicity is something the reader would not see
  anyway: its events are per table and are applied separately, and a guarantee
  which cannot be delivered all the way is not worth buying with the order locks
  are taken in;
- **nothing gets into the transaction until the stream has ended cleanly**: a
  broken stream or a rejected message leave it exactly as it was, and the post
  is simply retried. Exactly the same rule as `BulkWrite`'s;
- rows are parsed **on the post**, not on the commit: we reject a broken entity
  while the client is still writing;
- **exactly one** field is set in an action's envelope: zero - a message about
  nothing, two - a message whose order relative to itself is undefined, and the
  order is the one thing a transaction must not guess;
- the order of the actions is preserved, only **neighbours of the same kind**
  are merged. Ten `DeletePartitions` in a row cost a subscriber one instruction,
  while `write, delete, write` on one key arrive as three - otherwise the
  subscriber would be left with the wrong row;
- `CommitTransaction` first takes the transaction out of the registry and then
  applies it: from that moment nothing more can be posted into it, so exactly
  what was accumulated is what is applied;
- `CancelTransaction` **does not object to an unknown id** - what was
  accumulated never touched the table even once, so the client can always cancel
  in its own `finally`. `Commit` and `Post`, meanwhile, answer `not_found`. For
  the same reason the client's `commit` takes the handle **by reference, not by
  value**: the id lives only inside the handle, and a commit which ate it on its
  own failure would leave an open transaction on the server with nothing left to
  cancel it with.

## Clients

The core, the macro and both clients live in a separate repository -
`my-no-sql-grpc-sdk` - and the server depends on them by tag. The direction is
exactly this one, and the reverse does not work: the core is needed by both
sides, so if it stayed here and the SDK pulled it by tag, the server's test
binary would end up with **two copies of the core** (path and git - for cargo
these are different crates), and a `DbRow` from one would not fit a function
from the other.

There the core is cut into two crates, as in the JSON version: `abstractions` -
the entity traits, the protobuf codec and the schemas, `core` - the data model
(tables, partitions, rows). The reason is not beauty: a crate with entities,
shared across every service, has to take only the first and not compile the
table engine. The macro generates paths through the facade
(`my_no_sql_grpc_sdk::abstractions::...`), so an application has **one**
dependency; before the facade it would have had to declare the core as well,
which it names nowhere.

**`.proto` is compiled here on the spot, and the SDK holds a copy of it, not a
download by URL.** The download (`ProtoFileBuilder` with an http base, as in
`my-service-bus`) requires `ci-utils` with the `with-tls` feature, and that one
drags `flurl` - and with it a second instance of `my-http-utils` **without** the
`server` feature - into the build-dependency graph of everyone who depends on
the clients. In that shape `my-http-utils` does not compile, which means the SDK
would break the build of **this** server. A copy drifting from the contract is
caught by a CI step in the SDK, not by the build.

**The 30 s deadline hangs on the unary requests, not on the channel.**
`Endpoint::timeout` is raced against the *response* future, while three calls of
this same crate - `BulkWrite`, `UploadBackup` and the transaction post - are
client-streaming: the server stays silent until it has read the whole request. A
channel deadline, then, would cover the whole upload, and a batch which needs
more than thirty seconds on the wire would **never** go through, however many
times it is retried. The server streams would have survived it (they resolve on
the response headers), but relying on such a difference inside the channel is
not worth it: a deadline is put where it means something.

### An entity is declared by a macro

```rust
#[my_no_sql_entity(table_name: "traders")]
pub struct TraderEntity {
    #[proto_no(5)]  pub amount: f64,
    #[proto_no(6)]  pub instrument: String,
    pub computed_locally: u64,   // no proto_no - does not go on the wire
}
```

This is an **attribute** macro, not a derive, and for exactly this reason: a
derive can only look at the struct, and what is needed is to **write** the
service fields into it. The macro puts `partition_key` (1), `row_key` (2),
`time_stamp` (3), `expires` (4) ahead of everything declared - so an application
cannot miss the contract, forget a field or number it differently.

- **the numbers are written by hand, not counted from the declaration order**:
  the number is what a stored row is read back by, and it has to survive a field
  being inserted in the middle. A `proto_no` below 5 and duplicates are a
  compile error;
- **only what is marked up is serialized.** A field without `proto_no` is an
  ordinary field of an ordinary struct, it is never written and after
  `from_slice` stays at its default;
- `serialize`/`from_slice` are generated on top of our own protobuf reader from
  the core, so **`prost` is not needed for entities at all**, and `prost-types`
  is not needed by the server either - it is cut out of the dependencies
  together with the descriptors;
- proto3 semantics: a default does not go on the wire. Hence `TimeStamp` is
  absent until somebody sets it, and the server stamps its own clock.
  `Option<T>` is the exception: it is written even as `Some(0)`, because that is
  the whole difference between a zero and "not set";
- an unknown field number is **skipped** on read (the row was written by a newer
  build), while a known number with the wrong type is an **error**: silently
  substituting a default is the worst of all.

### Schema

A schema exists for **one** thing: to show a stored row as JSON. The server does
not see the macro - it has the row's bytes and the schema; it walks the schema,
writes a name for every field, reads the value by the declared type and writes
it through `my-json`. Nothing else.

```text
enum Scalar   { Bool, I32, I64, U32, U64, F32, F64, String, Bytes }
enum ItemType { Scalar(Scalar), Object(u16) }   // u16 - the index into Schema.messages
enum Tp       { Item(ItemType), Array(ItemType) }
struct Field   { no: u32, name: String, tp: Tp }
struct Message { name: String, fields: Vec<Field> }   // fields in ascending no
struct Schema  { root: u16, messages: Vec<Message> }  // messages by name
```

**Invalid states are inexpressible, and the enums are nested exactly this way
for that.** `ItemType` has no `Array` variant - which means an array of arrays
cannot be written, and protobuf has no `repeated repeated`; `Array` carries its
own element type - which means an array of nothing cannot be written. These are
not runtime checks: these cases simply do not exist. `Object(u16)` is a
**reference** into the message table, never a nested message: one message which
two fields reached is one entry, and a reference to itself stays a finite number
of bytes.

**The bytes are canonical by construction**: fields sorted by number, messages
by name, every singular field of the frame with exactly one occurrence - even
when the value is the default. Two processes which declared the same entity get
a byte-identical schema, and `from_slice` rejects any other spelling. This is
not pedantry: the server compares schemas **by bytes**, and a second spelling of
one schema is a second answer to the question "is this the same schema".

**The format is deliberately narrower than protobuf.** Nine scalars, not
fifteen: the macro is the only thing that produces a schema,
`macros/src/field_type.rs` maps Rust into exactly these nine, and nobody can
write `sint*`, `fixed*`, `sfixed*`, enums or maps. With a `FileDescriptorSet` a
third party is entitled to send `sint64`, and the renderer has to be able to
handle it; with our format that is **inexpressible**, and six `FieldKind`
variants, `Enum` and the whole of `write_map` are deleted from the renderer as
branches with nothing to produce them. Such a narrowing is possible only because
the format is ours: someone else's format cannot be narrowed, only parts of it
can be refused - and that is already a runtime error instead of an absent case.

The wire type already says whether this is a varint or a fixed width, so the
schema does not repeat it. It answers something else - "integer or fractional"
and "signed or not" - and this is exactly what tells `F64` from `U64`, even
though both arrive as type I64.

Field names are PascalCase: the four service ones are spelled that way by the
contract, and a single JSON object with two spellings inside it reads as a bug.

**`PartitionKey` (1) and `RowKey` (2) are described as ordinary fields**, and
`TimeStamp` (3) and `Expires` (4) are not. The keys lie in the row and are shown
like everything else, so the renderer never has to know a single reserved
number. The two moments are not in a stored row at all - the server writes them
on the way out - and a schema naming them would describe something that is never
there; their names come from the contract, by the same fallback they are named
by without a schema.

**`SchemaId` is a compile-time constant.** The macro produces a `const
SCHEMA_ID: u64` per entity and per message: FNV over its own `(no, name, tp)`
plus the `SCHEMA_ID` of the types its fields reference - a Merkle hash, that is.
A change inside a nested message changes its constant, and with it the constant
of everything that carries it, all the way up to the entity: a row written
before the change keeps an id nobody produces any more - exactly what the rule
"every row remembers its own schema" needs. And on the client **nothing** is
hashed at runtime.

**The server does not recompute the id.** One hash, one side, nothing to drift.
In exchange it **compares the bytes**: an id arrived which **this table**
knows - the stored schema and the arrived one have to match, otherwise the write
is rejected. Without this the id is an agreement of our client alone: a second
one, written against the published proto, is entitled to put its own constant
there, and the server would start showing the rows of one table through the
schema of another - silently and forever, because the id goes inside the stored
rows. The schema travels over this wire anyway, so the comparison costs less
than receiving it. The rule is written down in the comment on
`EntitySchemaGrpcModel` in the proto - whoever writes a second client has
nowhere else to take it from.

**The price, accepted by the project owner: recursion becomes a compile error.**
A constant cannot reference itself, so a message carrying itself, and two
carrying each other, do not build. A row of a NoSQL table is a flat record; a
tree is modelled by a second table or by a `Vec<u8>`. The direct case the macro
sees and rejects with a `compile_error!` naming the type; the mutual one it does
not see at all (the macro looks at one struct at a time), and there the constant
cycle itself catches it - `cycle detected when evaluating`. The message is
unintelligible, so it is written down in the macros crate's docs. As a side
effect this removes the way a friendly client had to overflow the renderer's
stack; the 100-level limit stays - it is against hostile input, and a schema
arrives as **bytes**, and bytes are not a declaration.

The writer is generic over the entity: the schema is built **once** per writer,
not per write, and its id costs nothing at all - it is a constant of the type.

Supported are `String`, `bool`, `i32/i64/u32/u64`, `f32/f64`, `Vec<u8>` (that is
`bytes`, not a repeated number), **nested messages** and any of them inside
`Option<>` and `Vec<>`.

**Two findings of the previous review this format removes, rather than fixes.**
The first - an empty package, which gave an unresolvable name `..Entity` and a
silently vanished JSON view: there are no packages, a schema names its own root,
and the `RootMessageName` field is deleted from the contract (the number is
`reserved`). The second - two different messages with one simple name,
collapsing into one: a name inside a schema is unique by construction, and a
reference is not a name but an index, so there is nothing to collapse. This does
not cancel the check in the builder: two different field lists under one name
still bring the builder down, because it is still Rust that declares them, where
two modules are entitled to name a struct the same.

#### The cache of parsed schemas

Reading a schema and resolving every reference in it is work which cannot be
done per row of a response, so the parsed index lies in `JsonSchemasCache` under
the id **the client named**: the server still hashes nothing.

- **the cache is one for the whole server**, not one per namespace and not one
  per table. The id is a hash of the shape, and a table rejects a second shape
  under a known id, so two different indexes under one key would mean a client
  which invented the number - and such a client is already showing its rows
  under someone else's names, and a second cache does not fix that;
- **a miss is cached too.** A blob which turned out not to be a schema would be
  parsed again on every row - the same work for the same answer. The rows are
  shown by field numbers meanwhile, as they are without a schema at all.

### Nested messages

```rust
#[my_no_sql_message]
pub struct Limits {
    #[proto_no(1)] pub max_lots: f64,
    #[proto_no(2)] pub allowed: Vec<Instrument>,   // and a message inside a message
}

#[my_no_sql_entity(table_name: "traders")]
pub struct TraderEntity {
    #[proto_no(5)] pub limits: Limits,
    #[proto_no(6)] pub history: Vec<Limits>,
    #[proto_no(7)] pub fallback: Option<Instrument>,
}
```

- **a separate macro, not the same one.** A message is not an entity: it has no
  keys, no table and no schema of its own. Hence the numbering **from 1**: the
  four reserved ones belong to the entity, and a message has nothing to occupy
  them with;
- **a message has no package, and nor has anybody else.** A message is declared
  inside the schema of the entity which carries it; one and the same message
  under two entities is two entries in two schemas, and that is right: they are
  two schemas;
- **it is declared next to the entity, not inside it.** One message which two
  fields reached is one declaration, and what a field references does not depend
  on which field got to it. Deduplication is by name, in the builder. But the
  deduplication **compares the shape**: one name with two different field lists
  is two different messages, of which the second declaration would be lost,
  while both fields would be shown by the first (a `double` read as a `String`,
  and nobody said so). Rust allows two crates or two modules to name a struct
  the same, and this is the only place where that stops being harmless - so a
  conflict brings the builder down, and the builder lives in a `OnceLock`, which
  means it will be heard about on the very first write. The entity's own name
  takes part in the check on equal terms with the rest: it is a schema message
  just like them, and a field which pointed at it would resolve to it;
- **a message carrying itself does not build** - see "Schema": its `SCHEMA_ID`
  would be an input to its own definition. This used to be a stated property of
  the builder, and what stopped an infinite declaration was that same
  deduplication by name; now such a declaration simply does not happen, and the
  deduplication stays for its first job - one message for two fields;
- a message **is always written**, even a default one: a message in protobuf has
  explicit presence, and a field of a struct is a value somebody put there. "Not
  set" is written as `Option<>`, and its absence is what does not go on the
  wire;
- repeated messages are **never packed** - packing exists only for varint and
  fixed width;
- **an unknown type is now taken to be a message**, not an error: the macro sees
  one struct at a time and cannot look at the type a field names. So that a typo
  does not turn into an unintelligible trait bound, the primitives protobuf does
  not have (`u8`, `u16`, `usize`, …) are rejected by name, and so is **any type
  with generic arguments** - neither `HashMap` nor `Box` can be a message,
  because both macros declare an ordinary struct. The exception is one and the
  same at every depth: `Vec<u8>` is `bytes`, not a repeated number, and inside
  `Option<>` and `Vec<>` too;
- **a generic struct is rejected out loud by both macros.** Both rewrite the
  declaration into an ordinary struct, so the type parameters, the lifetimes and
  the where-clause are lost along the way, and the caller used to get a trait
  error about a declaration they never wrote. And there is nowhere to carry them
  either: the schema is a static of the type and the table name is its constant,
  which means the contract has nowhere to hold a schema per instantiation.

The server needed nothing for this: it has rendered a nested object by schema
ever since it renders at all - all that was missing was somebody to produce such
a schema.

The reader holds a table cache - it is the `DbTable` from the core, the same
structure the server has: the reader's cache is exactly that. While a
re-subscription is in flight, the cache **keeps answering with what it has**: a
reader which handed out a stale row is doing its job, and a reader which
suddenly answered "there are no rows" is not.

- the read statistics accumulate **as a map by partition key**, not as a list:
  only a successful `GetChange` clears it, and the application is reading all
  that time - a list would grow at the rate of the calls for the whole length of
  an outage, while a merged map grows by the number of distinct partitions. The
  JSON version does exactly the same. Row keys are merged as a set, `Expires` is
  won by **the last one named** (in a sliding expiry the newest moment is what
  matters), and an untouched field still means "do not touch" and does not
  cancel an instruction already accumulated;
- `wait_until_initialized` waits on a `watch`, not on a `Notify`: the snapshot
  arrives **once per session**, and `notify_waiters` leaves nothing to whoever
  started waiting a tick later - there will be no second announcement to pick it
  up. A `watch` remembers the version the waiter started from, so the
  subscription goes before the flag is read and a snapshot which landed between
  them counts all the same.

### Namespaces

`GetNamespaces`, `DeleteNamespace`, `MoveTableToNamespace`.

- **default cannot be deleted**: a client which does not name a namespace has to
  land somewhere, so this is not a namespace like the rest - it is the place
  everything unnamed goes to, and it is always there;
- **the name is checked where it is resolved** - in `get_or_create` and in
  `get`, not on every call: this is exactly the place where a string from the
  request stops being a string and becomes a path (`<root>/<name>`,
  `create_dir_all` on creation and `remove_dir_all` on deletion). The check is
  the same one the folders get at start-up, and both sides of the resolve have
  to answer the same way: a name which cannot be created cannot be found either.
  A name made **of dots alone** is rejected separately: `.` and `..` pass every
  other check, and `..` is a level outwards. And the harmless form matters here
  no less than an attack: a name with a space in it or longer than 64 characters
  would create a folder the next start-up skips - every row written into it
  would become unreachable, and nobody would have said so;
- a namespace created **on the fly** first scans its own folder
  (`prime_for_writes`): it may not be empty - the same namespace in a previous
  life of the process, or one deleted and named again. Only the scan knows which
  versions are already on disk; without it a fresh namespace starts from zero,
  that is **below** what lies there, and the next start-up's deduplication (the
  higher version wins) would bring the old partitions back, cancelling
  everything written. Start-up does not do this - its own load is that same
  scan, and there is no point scanning twice;
- moving a table **writes the rows across, it does not hand them over**: a
  namespace owns its own folder, so the data is written into the recipient's
  files. The schemas need no move of their own - they are the table's
  attributes, and the attributes are copied into the table the recipient
  publishes;
- so the table **disappears from the source first**, and the rows are read from
  an `Arc<DbTable>` already taken out, while at the recipient it is published
  **only once filled**. Every RPC is its own task: while the source is being
  resolved by name, a row can be written into it which will be confirmed,
  announced to the subscribers and destroyed right away together with the
  source; and an empty published table at the recipient would accept a write
  which `CleanTableAndInsert` would wipe. This is the same argument
  `delete_table` stands on. For the reader the order is kept as it was - the
  event to the recipient goes out before the event to the source, so somebody
  watching both will not hear that the table is nowhere;
- deleting a namespace takes the folder down whole: the tables are already taken
  out, but nobody will clear the persistence queue of a namespace which can no
  longer be reached. This is done **under `persist_lock`**: a persist pass which
  took the list of namespaces before the deletion may be inside the files of
  this folder right now, and every open there is an `.expect(..)`, that is, a
  panic of the timer;
- **a folder which did not get deleted is an answer to the caller**, not a line
  in the log: the namespace is gone from memory either way, there is nobody to
  retry, and the folder left behind is a full copy of the data which the next
  start-up will load back. Saying "deleted" would be the one wrong answer.

## GC

A timer every 30 s, a pass over the table:

- rows whose `Expires` has come and gone (`0` = never);
- eviction of partitions by `max_partitions_amount` and of rows by
  `max_rows_per_partition_amount` - the ones which go are **those nobody has
  read for the longest**. That is exactly what the last-read mark is there for,
  and what moves it is the reader's read statistics.

The `CleanAndKeepMaxPartitions` and `CleanPartitionAndKeepMaxRows` handles do
the same thing right now and with a number of their own. A negative limit is an
error, not "no limit": a typo by the caller would otherwise turn into an
operation which silently did nothing.

Decisions:

- **a cheap look under the read lock first**, and only if it found something -
  everything again under the write lock. On almost every tick there is nothing
  to do, and a table nothing expires out of should not be locking its writers
  out thirty times a minute. Planning and applying under a single taking of the
  lock is what keeps a row somebody wrote or read in between from being deleted;
- **partitions are evicted first**: the rows inside them are gone before anybody
  looks at rows at all. A subscriber which has been told "the partition is gone"
  is not told about the rows which were in it afterwards;
- **a row which is expiring counts towards the limit**: eviction would otherwise
  throw out one more on top of it, and more would leave the partition than the
  limit asked for;
- **an expired partition goes whole** and before every other one: before anybody
  looks at rows at all, and before the limit - so that it does not take up room
  in a limit it is about to leave.

### Schema GC

On the same tick, after the rows: a pass which has just taken out the last row
of some entity version is exactly the one whose schema has nobody left to name
it. Without it the leak would have moved from `schemas.bin` into `tables.meta`:
the registry could register/contains/get and nothing which deletes - a table
re-created a hundred times under a changing entity would drag a hundred dead
schemas along, and they would be loaded at every start-up.

- **the cheap exit first: one schema or fewer - return.** That is 99.95% of real
  tables, and it is obliged to cost O(1) - the same shape as the cheap look in
  the row GC;
- more than one - **the live `schema_id`s are collected by walking the rows
  under the read lock**. There is no early exit here by nature: proving a schema
  is not needed means seeing every row. The walk only reads, though, and
  throwing the dead ones out is a swap of the attributes;
- **a table nobody has written to is not walked again**: the moment of the last
  check is remembered and the table skipped until `lastWriteAt` has stepped past
  it. A genuinely mixed table would otherwise pay for a full scan every 30
  seconds forever. The moment is taken **before** the walk, so a write which
  landed while it was running reads as the newer one and brings the next pass
  back; that same write, under the attributes lock, cancels the current one -
  its row could have named a schema the pass was about to drop, while
  `register_schema` said nothing because the schema was still there. The cost of
  getting this wrong is bounded anyway: the schema arrives with **every** write,
  so the next write of the same version brings it back;
- **but one walk happens regardless, even for a table nobody has ever written
  to.** "Not checked yet" and "checked, and not written to since" are different
  states, and what tells them apart is a separate value of the check moment. A
  table brought up from disk with its dead schemas would otherwise never meet
  the condition: nobody is obliged to write to it, and the dead schemas are
  exactly what it carried over from the process's previous life and loads at
  every start-up. That is, the leak the schema GC exists for would have outlived
  it;
- **the last schema is never thrown out, even when there are no rows at all.**
  HTTP is the surface for the UI, and a UI showing a table which has gone empty
  still draws its columns from the schema. The rule: schemas without rows are
  thrown out **while more than one remains**; which one survives is decided by
  the largest id, so that two servers with the same data answer the same way.

The subscribers are told nothing about this: a reader's cache is rows and the
table's attributes, and the schemas are not among the attributes it is sent -
showing a row as JSON is something only the server can do. The disk is told: a
dropped schema is a `tables.meta` write, like any other edit of the metadata.

### TTL renewal on a read

`GetChange` carries the read statistics: the last-read marks and the **new
expiry time** of the partition and of the rows. A reader which keeps asking for
a row is what keeps it alive - there is no separate call to pay for that, it
rides along with the one the reader makes anyway.

- each of the two has **a flag and a value**, not one number: "leave it alone"
  and "set it to never" are opposite instructions, and `0` can only mean one of
  them;
- a row's `Expires` is part of the stored row, so changing it is marked for
  persistence. But **not urgently** (`Sec15`): TTL renewal is the most
  repetitive write there is, and a late moment lets a run of them merge into
  one;
- a partition's `Expires` **is not written to disk** - the same as in the JSON
  version. Readers set it on every call of theirs, so a restart loses at most
  the time until the next one; persisting it would mean versioning the blob
  format for the sake of a value which will be stated again seconds later.

**Found incidentally by a test:** a partition's `content_size` was counted from
the size of the row together with `Expires`, and that now changes on the fly -
the subtraction when a row was deleted went into an overflow. A partition
accounts by `get_stored_size()` - the part which can not change.

**A known cost:** in the worst case the cheap look walks the rows (exiting at
the first one it finds). An index of expiry moments per partition would make
that O(1) - that is what to reach for when a profile shows it is visible, and
not before.

## MigrateFrom

Pouring a table over from another server of this kind. **The schema travels
together with the data** - the very rule everything else here lives by: the
recipient puts a schema it has never seen into **the table** it is filling, and
every row keeps a reference to it. Nobody moves a schema by hand, and a table
poured over is showable on the other side. It can not arrive through the
attributes: `GetTables` gives out what the table is **configured** to be, while
the shape of its rows is a property of the data, so it travels with them.

- `GetRowsWithSchema` gives the rows out **grouped by schema**, and that is
  exactly the same shape a write arrives in (`Schema` + `Rows`). What one server
  gives out, the other takes in. A table whose rows were written under different
  entity versions leaves as several chunks - one per schema;
- a chunk **without a schema** is the rows whose schema the source no longer
  has. They travel anyway: losing a row because its schema went missing is worse
  than moving it unshowable;
- **the recipient pulls**, rather than the source pushing: an inbound route is
  then only needed to the source. The url comes from the caller, though - which
  is worth remembering when the caller is not an operator;
- everything is gathered whole and applied through **a single entry**,
  `CleanTableAndInsert`: the chunks are cut by schema, and each of them on its
  own is not a state the table was ever in;
- the attributes that arrive are the ones that were there: a migration which
  reset them to the default would silently have turned persistence or a limit
  off;
- a row's `TimeStamp` is preserved - it is the same row that was there, not a
  new one.

## Backups

**A backup is a zip of one namespace**, laid out the way the namespace itself
is:

```text
<table>/.metadata               the table's attributes - together with its schemas
<table>/<base64url(partition)>  the partition - the same blob that goes into a slot
```

`<table>` is the table's own name while that name can be a path segment, and
`~<base64url(name)>` when it can not.

A partition is already serialized into a self-contained blob for the page-files,
so a second way to write one is not needed: a backup is an array of partitions,
one entry each, with what they can not be read without beside them. And it is an
ordinary zip - the operator opens it with the tool they already have.

- **one archive per namespace, not one for the whole server**: a namespace is
  the unit everything else comes in (its own tables, its own folder). Restoring
  one namespace is one file, and `MaxBackups` is counted per namespace,
  otherwise a noisy one evicts a quiet one's snapshots;
- **base64url for the partition key, not standard base64.** The standard
  alphabet has `/` in it, and that is the path separator inside a zip: a key
  which encoded with one in it would turn `traders/<key>` into a nested folder.
  (The JSON version has standard base64 here - we are not copying that.);
- **the table name is encoded with the same base64url - but only when it can not
  be a path segment.** Table names are validated nowhere, and a `/` inside a
  name made two levels out of `eu/traders/<key>`: `read()` cuts at the first `/`
  and hands the rest to the key decoder, which, failing to make sense of it,
  ends the read of the **whole** archive - one such table made every correctly
  named one beside it unrestorable as well, and the backup timer reported
  success while writing that. An encoded name carries `~` at the front: that
  character is not in the base64url alphabet, so a name without it is a name and
  not a guess, and archives taken earlier read as they always did. An ordinary
  name stays itself - a plain zip is plain precisely so that the operator opens
  it and sees which table is which. `..` is encoded too: it is we who read the
  archive into memory, while the operator unpacks it with an ordinary tool;
- **two backups of the same second are two files.** The name holds the second,
  and a manual snapshot lands in the same second as the timer's one more often
  than it seems: the second and later get `_02`, `_03`. Without this the newer
  one silently overwrote the older, and `MaxBackups` counted the two as one. The
  name is claimed by creating the `.tmp` through `create_new`, so two which
  started at once end up with a name each rather than with whoever wrote last;
  the suffix sorts after the bare name and before the next second's, so the
  order of the names is still the order of the events. It counts up to `_99`:
  the hundredth snapshot of one namespace inside one second is not a backup but
  a loop in the caller, and it is answered with a refusal rather than with a
  name from the next second;
- **a failed backup write is a `Result`, not a panic.** Persistence panics on a
  failed write deliberately, but the backups disk is not the data disk: a
  `BackupsDest` which has filled up must return `BackupFailed`, which the caller
  already knows how to handle. A panic takes the timer's task down with it, and
  then the next backup does not happen either;
- the blobs go in **uncompressed** and the zip does the compressing: zstd on
  each blob plus deflate on top would pay for the compression twice and save it
  once;
- the snapshot is taken **from memory, not from the page-files**: the disk lags
  by a sync period, and what is needed is a backup of what the server answers
  with right now;
- **an empty namespace is not written at all**: an empty archive taken every
  interval would take up a `MaxBackups` slot and evict a real one;
- what is restored **replaces** rather than merging - rows nobody can explain
  would otherwise stay behind. One partition can be restored on its own; the
  schemas travel every time, because a partition without its schema is a
  partition there is nothing to show it with. They arrive in the table's
  attributes, and `set_attributes` **merges** them, so restoring into a live
  table adds what the archive knew without cancelling what the table learned
  after it;
- the backup's name comes from the caller and becomes a path, so anything with
  `/`, `\` or `..` in it is refused rather than resolved. The same goes for the
  namespace's name;
- **download and upload** are the same bytes in both directions, as a stream. An
  uploaded archive is *placed*, not restored: restoring is a separate call, and
  an archive worth keeping is worth looking inside first. Something which is not
  a zip never reaches the folder;
- **`BackupsDest` is a setting of its own, not a folder inside the persistence
  root**: every folder there is loaded as a namespace, and `backups` would have
  become the namespace "backups". Not set - there are no backups, and every call
  says so;
- the timer runs only if both `BackupsDest` and `BackupIntervalSecs` are set;
  `MaxBackups` keeps the last N. How often to back up and how many to keep is
  the operator's policy.

## Persistence

As in the JSON version: slotted-page, `crc32|version|body_len|table_len pk_len
table pk payload`, size classes in powers of two from 512 B, the free-list in
memory only, recovery = a scan, deduplication by `version`, vacuum with compact
in copy-fsync-truncate order. The partition blob is a binary format of its own
(not proto): `format | rows_count | (schema_id, len, row)*`, zstd on top.
`tables.meta` is YAML, and the table's schemas live in it too (base64url, one
per line, in ascending id order: a file that has not changed in meaning must be
written with the same bytes).

- **marking for persistence does not consult the `persist` attribute** - the
  pass itself consults it. The queue records *what changed*, and what the disk
  is supposed to hold because of it is decided when the queue is drained, while
  the table can still be looked at. Otherwise deleting a table whose persistence
  has been turned off queues nothing at all: both the slots and the
  `tables.meta` entry stay, and the start-up load does not look at the attribute
  at all - so a table deleted before the restart is there after it, in place and
  with every row. That is why a pass on `persist: false` **frees** the slot
  rather than bailing out: a table that is not persisted must hold nothing on
  disk, including what it wrote while the attribute was still on. The price is
  one idle `delete_partition` per touched partition per sync period for a table
  that lives only in memory;
- **`tables.meta` is written for a table without persistence too**: it is the
  registry of which tables exist, what they are configured for and under which
  schemas they were written, and a table that is not in it is not brought up by
  the next start. A schema that could not be decoded is skipped, not allowed to
  bring the start-up load down: it costs showing the rows of one version, while
  a refusal would cost the rows themselves. The same argument by which
  `set_table_attributes` writes the metadata even when it is turning persistence
  off;
- **the table name and the partition key are no longer than 65,535 bytes**, and
  that is a limit of the format, not a policy on top of it: both lengths sit in
  the slot as `u16`. A longer key was written with a truncated length, and the
  crc is computed over the buffer *as it was written*, so nothing complains on
  the way back: the slot decodes into the shortened key, and the tail of the
  real one ends up glued to the front of the payload. It shows only at
  recovery - the start falls over on an unknown blob format or, with
  `SkipBrokenPartitions`, loses the partition silently. A refusal at the moment
  of the write is the one of the three outcomes that is noticeable when it
  happens;
- **the number of records read out of the blob does not set
  `Vec::with_capacity`.** The same `partition_blob` is read both out of a
  page-file and out of an archive an operator uploaded, and a failed memory
  allocation is an abort, not an error one can tell anybody about: five bytes,
  `01 FF FF FF FF`, were enough to bring the process down on `inspect`. We read
  into an empty `Vec`, and the very first read past the end of the body says
  there is no body;
- **the vacuum goes round once an hour, the timer tick once a minute.** It
  rewrites page-files holding the `FilesRepo` mutex for the whole pass, and a
  persist pass waits for that same mutex **holding `persist_lock`** - that is,
  every other namespace's queue stands behind it. The moment of the last pass
  lives in memory only and is seeded with the moment of the start: a restart is
  exactly the thing that rewrote nothing, and a file with a stamp that nobody
  but itself reads is not needed. As in the JSON version.

### Flush on demand

`FlushToDisk` (Writer gRPC) writes everything the queue holds **right now**,
whatever sync period each change asked for. It answers how many tasks that was.

- the queue is **taken in one go on entry to the call**: a write made while the
  flush is running belongs to the next call. "Drain until the queue is empty"
  never ends under a live writer, and an operator is asking exactly one thing:
  "is what I wrote before I called already on disk?";
- **server-wide, with no namespace** - like a backup. The question "is the disk
  current before I stop this" is never about one table. And an empty string in a
  namespace name already means *default*, so "all" is not something you can say
  with it;
- `persist_lock` on `AppContext`: the timer, the flush and the drain on shutdown
  write into the same page-files, and a partition that landed before the
  `tables.meta` entry about its table is a table with default attributes on the
  next start-up load;
- we put nothing out to the reader: nothing in memory changed.

## Where it listens

Two listeners, two ports, both on `0.0.0.0` by default: `8000` - HTTP, `8888` -
gRPC. They are overridden by environment variables - `LISTEN_HTTP_ENDPOINT` and
`LISTEN_GRPC_ENDPOINT` - and both take a whole **endpoint** (`127.0.0.1:8888`),
not only a port: narrowing gRPC down to loopback is a deployment's decision, not
the server's, and there is no way to express it without a host. A bare port
(`9100`) is accepted too and is read as that port on `0.0.0.0`.

A value that parses neither as an endpoint nor as a port **brings the start
down** - it does not fall back to the default. Whoever wrote the variable meant
to be somewhere else, and a server that quietly came up on the default port is a
server nobody will find.

They are resolved **once**, in `AppContext`: a listener does not move after it
is bound, and `/api/Status` returns `httpEndpoint` and `grpcEndpoint` - where
the server actually stands. Returning a port number would be a half-truth in
exactly the deployment where the host is the part that matters.

## HTTP

The HTTP port is **a surface for the UI**: reads rendered into JSON by the
schema, plus those writes which do not carry an entity. It exists for the UI's
sake - to look at the data and to call a schema-less write; there are no ported
runbooks aimed here, and compatibility with the JSON version's HTTP is not a
goal.

**An entity is not accepted over HTTP.** The JSON would have to be turned into
protobuf, and that needs a schema - and here a schema travels **together with
the write**, and that is a gRPC contract. Keys and attributes need no schema at
all, and HTTP is exactly where they belong: they are called by hand, from curl,
from a dashboard. (One exception - MCP on `/mcp`, where the schema is taken from
the table's attributes, not from the request. Why that does not retire the rule
is in "MCP".)

**What is exposed over HTTP is exactly what the UI shows, and that rule has been
the same one from the start - what changed is the premise, not the decision.**
While there was no UI there were no addresses for backups and namespaces either:
there was nobody to show them to, and the gRPC port is not exposed outward, so
the operator already had a way in, and a second one would have meant another
door to the same operations. The UI arrived and shows both - so HTTP now has
`GET /api/Namespaces/List`, four backup reads (`/api/Backup/List`, `/Tables`,
`/Partitions`, `/Rows`) and three of its writes (`/MakeBackup`,
`/RestoreFromBackup`, `/RestorePartition`), as well as `GET
/api/Partitions/Details`, `GET /api/Row/Download` and the settings page
(`GET`/`POST /api/Settings`, `POST /api/Settings/UiWrites`).

Still not exposed is what the UI does not show: `FlushToDisk`, `MigrateFrom`,
downloading and uploading the zip itself, `DeleteNamespace`,
`MoveTableToNamespace`, the GC handles (`CleanAndKeepMaxPartitions`,
`CleanPartitionAndKeepMaxRows`) and `GetTableSize`. The sizes and counters
people look at with their eyes are answered by `/api/Status` and `/metrics`:
that is a different question - "what is with the server right now" - and a
different shape of answer.

**The three backup writes are closed by the UI write window**, the way the MCP
tools are closed by their own. Two windows - two decisions: opening writes for
an agent does not mean unlocking the buttons on a page somebody left open. Their
mechanism is one - `WriteWindow` in `server/src/app/write_window.rs`, ten
minutes, in memory only - and one written figure instead of two: the MCP window
used to live in an atomic of its own with its own constant, that is, one
decision written down twice.

**The shape of the answer answers to the UI, not to the JSON version.** `GET
/api/Row` always returns an array, even for a single row: that is the very thing
the UI parses, and a shape which changes with the number of things found would
force it to be parsed twice. `GET /api/Partitions` returns `{amount, data}`,
where `amount` is the partition count of the **whole** table and `data` is the
window named by `skip`/`limit`: a bare array cannot say how many more pages
there are, and that is exactly what this route is asked. Both field names are
nailed down by a test - `PartitionsApiModel` in the UI reads them, and renaming
either of them gives an empty screen without a single line in the log.

What there is: `DELETE /api/Row`, `DELETE /api/Partitions`, `POST
/api/Rows/BulkDelete`, `POST /api/Tables/Clean`, `DELETE /api/Tables`, `POST
/api/Tables/Create` and `/CreateIfNotExists`, `PUT /api/Tables/Attributes`,
`POST /api/Mcp/Writes`, `POST /api/Settings/UiWrites`, `POST /api/Settings`,
`POST /api/Backup/MakeBackup`, `/RestoreFromBackup`, `/RestorePartition` - plus
the reads and the operations below.

`GET /api/Namespaces/List` deliberately does **not** read the `ns` header: it is
the very call which says which namespaces exist, and a typo in a saved name
would turn the list itself into a 404 - that is, there would be nothing left to
fix the choice with.

`GET /api/Partitions/Details` returns the same `{amount, data}` envelope as
`/api/Partitions`, but with the partition's metrics. The core does not hand out
per-partition metrics (`DbTableMetrics` is about the whole table), so the
numbers are taken from `RowStatistics`, addressed by row; and they are taken
**not** through `db_operations::read::get_rows`, which moves the last-read
marks: the page polls this route once every three seconds, and metrics which
touch the marks would save from eviction exactly those cold partitions they are
showing.

`GET /api/Row/Download` is the only route which takes the namespace from `?ns=`
and not from the header: it is opened by following a link, and an `<a href>`
cannot attach a header.

The backup reads resolve the namespace **by name**, not through a live
namespace: an archive outlives a namespace which is already gone, and a 404 on
the snapshots of a just-deleted namespace would hide the only copy of it left.

- **the render's nesting is capped at 100 levels**, past which the value is
  shown as base64. The render is recursive over the **data**, and a level costs
  two bytes of wire: a forty-kilobyte row is twenty thousand frames and an
  overflowed worker stack, and a stack overflow is an abort, not a panic a
  handler will catch. The row meanwhile is on disk and will do it again after
  the restart. A message carrying itself is something the macro will no longer
  build (see "Schema"), but the cap stays: a schema arrives as **bytes**, and
  bytes are not a declaration, so any writer can write down a shape no macro
  would have built. 100 is where protobuf implementations stop;
- **a namespace is resolved without being created** everywhere except creating a
  table. A typo in `ns` on a delete must not leave a folder on disk; and
  creating a table is exactly the operation whose job is to "bring into
  existence";
- **`ns` is read off the request, not off the parsed contract**: the header
  first, then the `?ns=` query parameter. The same as in the JSON version, and
  for the same reason: not every caller will attach a header (a browser download
  is an `<a href>`, and the hand reaches for `?ns=` sooner than for `-H`), and
  the resolution has to be one for all the actions. `#[http_header(name =
  "ns")]` stays in the contracts - swagger is built from them, and a header read
  straight off the request is invisible in it - but that is a description, not a
  source: reading the field instead of the request quietly switched the fallback
  off, and `DELETE /api/Tables?tableName=traders&ns=prod` took down the table in
  **default**;
- `Create` and `CreateIfNotExists` are **two routes, not a flag**. They are two
  operations: one says "this must not be here yet" (409), the other - "I do not
  care". A flag which changes what a refusal means is a flag somebody will one
  day set the wrong way;
- `CreateIfNotExists` **applies the attributes to an already created table too**
  (both on HTTP and on gRPC): it is the very call a service makes at start-up
  with the attributes it needs - otherwise a raised limit goes nowhere and the
  service keeps being evicted the old way, with nothing anywhere to explain it.
  `created` is kept, as it is in `PUT /api/Tables/Attributes`. A call which
  changes nothing does nothing: the attributes are compared, and a repeated
  start-up does not write `tables.meta` and does not wake the subscribers;
- `PUT /api/Tables/Attributes` **replaces** the attributes rather than patching
  them: what is not named goes back to the default, so the caller always knows
  what the table will end up as. `created` is kept - setting a limit does not
  mean creating a table;
- `BulkDelete` is a `POST` with a body, not a `DELETE`: a body on a `DELETE` is
  what proxies drop. The body is an **object** `{"pk": ["rk", ...]}`, because
  that is what it is - a map, and a map's keys do not repeat, so let the shape
  say so. It is parsed **before** anything is touched: a body the server did not
  read leaves the table as it was;
- `syncPeriod` is spelled with the same letters as in the JSON version (`i`,
  `1`, `5`, `15`, `30`, `60`, `a`): the enum is one and the same on both
  transports, and rewriting it here would mean introducing a second spelling of
  one and the same value.

#### Route names

Five routes are named differently here than in the JSON version - after the
controller they actually belong to.

| here | in the JSON version |
|---|---|
| `POST /api/Tables/Clean` | `PUT /api/Tables/Clean` |
| `DELETE /api/Tables` | `DELETE /api/Tables/Delete` |
| `DELETE /api/Partitions?partitionKey=` | `DELETE /api/Rows/DeletePartitions?partitionKeys=` |
| `POST /api/Rows/BulkDelete` | `POST /api/Bulk/Delete` |
| `GET /api/Row/Statistics` | `GET /api/Debug/GetRowStatistics` |

**The old spellings are not here.** They were - four as `deprecated_routes` on
the same action, `Clean` as a second action under `PUT`, `partitionKeys` as a
second name for the parameter - on the grounds that a `curl` written for one
server should work against the other. That is not a goal: the UI is what comes
here, and scripts carried over here which aim at the JSON version do not exist.
A route nobody calls is a spare line in swagger and a second spelling which has
to be remembered and fixed along with the real one. That the registered routes
are exactly the declared ones is held by a test in `controllers/builder.rs`.

### ApiKey

`ApiKey` in the settings. **Absent means no protection**, exactly as it was
before the setting existed: turning protection on is an operator's action, not a
quiet surprise on an update. The key arrives in the `apikey` header - the same
as in the JSON version, on the one route that version protects.

- it is **middleware**, not a check in every action: a route added tomorrow is
  protected by default, not because somebody remembered. And `/metrics`, with no
  `controller:`, cannot carry `authorized:` at all - through the macro it can be
  neither excluded nor included;
- it stands **after swagger**: a browser fetching `swagger.yaml` cannot attach a
  header, and the UI shows the shape of the API, but not a row. Calls **from**
  the UI go through the same door as everything else;
- **only `/api/IsAlive`** is open: it is pulled by a load balancer, which
  usually cannot be taught a header, and it says nothing but the name of the
  application, its version and the clock. It is matched **by segments**, with
  the same `HttpRoute` the router picks an action with: the router ignores a
  trailing slash, so `/api/IsAlive/` does reach the action - while a string
  comparison would answer it with a 401 and take out of the pool exactly those
  instances the exemption exists for;
- **`/metrics` is closed.** It names every namespace and every table and counts
  their rows - that is the shape of the data, even if not the data. And a key
  only ever appears because an operator wrote one in, which means the scraper's
  configuration is being written in the same breath; a scraper that was not told
  fails loudly, and that is the right failure;
- **`/mcp` is closed by the same key** - the middleware stands before it as it
  does before the controllers. MCP has no secret of its own and needs none: it
  hands out the same data and calls the same operations, and a key, the second
  one, is a second thing to rotate;
- the comparison is **constant in time**. A plain `==` returns on the first
  difference, and the time the answer took says how many bytes of the key were
  guessed. The length is compared first and does leak - the length of the key is
  not the key, and hiding it would mean hashing both sides for no gain;
- the same 401 for a missing key and for a wrong one: telling them apart would
  mean answering a question which must not be answered;
- the key is **not printed**: the settings' `Debug` is written by hand and shows
  `<set>`, and their `Serialize` is removed - a derived `Debug` on a struct with
  a secret stands one `{:?}` away from the log.

## Operations

Five handles for looking at a live server. All of them but flush are reads, and
all of them are on HTTP. None of them touches `FilesRepo` - its mutex is held
across file I/O, and a scrape that arrived during a vacuum would wait out the
vacuum.

**`GET /api/Status`** - everything the server can say about itself: the
settings, every namespace with its own tables, the persistence queue, the
connected readers, the open transactions, the MCP write window.

- **every namespace at once, with no parameter**. A namespace here is the unit
  of everything: its own tables, its own folder, its own persistence queue. The
  status of one of them is not the status of the server; and a monitoring handle
  that answers 404 to a typo in a name is one nobody needs;
- the only lock taken is the table's read lock, once per table, through the new
  `DbTable::get_metrics()`. The three numbers are taken in **one** acquisition:
  taken one at a time they are three locks and a picture where the rows belong
  to a different moment than the partitions holding them. The two old places
  (`GetTableSize` and `/api/Tables/List`) have been switched over to it as well;
- `schemasCount` is **on the table**, not on the namespace: a schema belongs to
  the table whose rows were written under it, and adding them up per namespace
  would mean counting one and the same schema of two tables twice. One table,
  one schema in about 99.95% of cases, so a two here is a signal: either a
  deployment is in flight, or two different entities are aimed at one table. The
  same number is in `/metrics` (`mynosql_table_schemas`);
- `mcpWrites` - whether the MCP write window is open and how much of it is left.
  The window lives only in memory and closes itself, so the only way to find out
  whether it is open is to ask; a window that got forgotten is exactly what it
  is supposed to prevent;
- `lastWriteAt` on a table is a new atomic in the core, set **after** the write
  lock is released, by whatever actually changed something. Absent if nobody has
  written to the table since the process started: the moment is not persisted,
  and restoring it off the disk would mean answering about the previous run;
- what is not there and why: **writers[]** (a writer here is anonymous - it
  holds a channel and does not introduce itself), "per second" counters (there
  is not a single counter - that is `/metrics`, not here), page-file sizes (see
  about `FilesRepo` above).

**`GET /api/Connections`** - the same reader rows, as an object with a single
key `readers`, not as a bare array: a bare array could not have grown a second
kind. The collector is shared with `Status`, so a session looks the same
everywhere.

- `ip` is taken from `remote_addr()` **before** `into_inner()` - that one eats
  the extensions, and there is no second chance. Behind an L7 proxy it is the
  proxy; there is no forwarded-for here, because there is nobody to set it;
- `lastIncomingSecsAgo` is a **number**, not `"1.523s"` as in the JSON version:
  that is the debug `Debug` of `Duration`, and it has to be parsed first;
- `pendingChunks`, not "bytes": the queue here is a list of instructions.
  Counting bytes would mean either walking the queue under the same mutex every
  write pushes into, or moving the size accounting onto the write path. Neither
  one is worth a number on a page.

**`GET /metrics`** - Prometheus, the text format, written by hand.

- **without the `prometheus` crate**: its value is the registry, and there is
  nothing to store. Everything is computed **per scrape**, not by a timer - and
  this is exactly what the JSON version does wrong: there a deleted table goes
  on reporting its last value until the restart, here it disappears the same
  second;
- label values are **escaped**. Table names are not validated anywhere, a client
  is entitled to create a table with a quote in its name, and one such name
  would make the entire scrape unparsable;
- readers are grouped by `(ns, app, version)`, not by session: a session id is
  issued on every greeting, and a label built from it would leave a dead time
  series behind after every reconnect, forever;
- the route has **no `controller:`** - without it the macro does not generate a
  description, and `/metrics` does not end up in swagger. The flip side: it
  cannot declare its own authorization either, so it inherits the global one -
  and that is exactly what is wanted from it: `/metrics` is closed behind ApiKey
  deliberately, see "ApiKey" above.

**`GET /api/Row/Statistics`** - why a row is still here or about to go: both
last-read marks (eviction sorts by them), both `Expires`, both sizes and the
`TimeStamp`.

- reading the statistics **is not a read of the row**: a handle that moved the
  mark would always answer "just now" and would save from eviction exactly those
  cold rows someone went to look at;
- `partitionExpires`/`rowExpires` are absent, not zeros: "never" and "at the
  epoch" are different things, and a whole contract here is built on that;
- `rowDataSize` is counted the same way the partition counts it (stored, without
  `Expires`) - otherwise the two numbers side by side would not add up;
- no partition and no row are **different** 404s: whoever asks "where did my row
  go" finds out whether the partition is intact.

## MCP

`/mcp` on the same HTTP port, behind the same `ApiKey`. The set is **the same as
in the JSON version**, in substance: fifteen tools and two prompts. This is its
surface for "a human asks about the data in words, an agent looks", and the
question it is asked is the same one, even though the rows here are protobuf.

| | |
|---|---|
| read | `get_namespaces`, `get_list_of_tables`, `get_rows` |
| reading backups | `get_list_of_backups`, `get_backup_tables`, `get_backup_partitions`, `get_backup_rows` |
| write | `insert_or_replace_row`, `bulk_insert_or_replace_rows`, `delete_row`, `bulk_delete_rows`, `delete_partitions`, `clean_table`, `move_table_to_namespace`, `restore_backup` |
| prompts | `mcp_writes_enable_policy`, `entity_schema_policy` |

Two divergences from the JSON version's list, and both of them because it has a
UI of its own and we do not:

- **`get_namespaces` added.** Every tool takes a `namespace`, and there is
  nowhere for an agent to look it up: over there the namespaces are on the
  screen. A parameter whose values the caller is obliged to guess is a parameter
  that will be guessed;
- **`paste_delete_via_ui` was not carried over.** It exists because its
  `bulk_delete_rows` can do one partition, and a wide delete had to be handed
  off to a UI dialog to do it in one go. Here `BulkDelete` takes as many
  partitions at once as you like - what that workflow was set up for is a
  property of the contract here - and there is nowhere to hand a delete off to.
  Its second half (showing a human the list before anything is touched) has
  stayed: it is written down as a rule in `mcp_writes_enable_policy` and in the
  descriptions of the destructive tools.

In its place - `entity_schema_policy`, a prompt about something the JSON version
never has to say at all: the rows here are protobuf, and JSON reaches them only
through a schema.

### The write gate

Writes are **closed**, and a human opens them: `POST
/api/Mcp/Writes?enabled=true` gives a 10-minute window, `?enabled=false` closes
it at once, and a restart always comes up closed. What is left of it is visible
in `/api/Status` (`mcpWrites`). The state is an atomic in `AppContext` and does
not go to disk: a window that survived a restart is a window that got forgotten.

- **what holds this shut is not a key, but the list of tools.** An MCP client
  can make exactly the calls that are registered here, it does not make
  arbitrary HTTP - which means a route that is not a tool is out of the model's
  reach. So the switch needs no second secret: it is enough **not** to declare
  it a tool. In the JSON version the same role is played by a button in the UI,
  with the same kind of ordinary route underneath it;
- **on HTTP, not on gRPC**, even though the operator's handles here live on
  gRPC. The switch is part of the MCP surface, and that surface is on HTTP; a
  second transport would mean that an operator who has a way in to the MCP
  itself cannot open writes for it from that same place;
- **a repeated call moves the end of the window, it does not add to it**: the
  question the window answers is "how much longer from right now", and two
  presses a minute apart must not give twenty minutes;
- the window is a **permission, not a check**: it says that the agent may write,
  and says nothing about what exactly. Showing a human the list before a wide
  delete is a rule of the prompt and of the descriptions, not a mechanism.

### Writing an entity: JSON → protobuf

This is the only place where an entity arrives **not** serialized, and it
refines the rule "an entity is not accepted over HTTP" (see "HTTP"). The
argument behind that rule was not "JSON is not allowed", but "JSON needs a
schema, and a schema travels together with the write". Here the schema **already
exists** - in the table's attributes - and is taken from there:

- **nothing here registers a schema.** A schema arrives with the entity over
  gRPC, under an id the client folded out of its own type; a shape typed in by
  hand has no right to be the first thing a table learns. The direct consequence
  of that: **a table nobody has ever written to cannot be written to from
  here**, and the answer says so outright, not "error";
- **more than one schema - the call refuses and asks for a `schema_id`.** Two
  versions in one table mean a deployment in flight; choosing for the caller
  means storing a row under a version they did not mean, and the row outlives
  the guess. `get_list_of_tables` returns `schemas_count` so that this is
  visible in advance;
- **a field name the schema does not have is rejected**, and the answer lists
  the ones it does have. An accepted row with a typo is a row stored without the
  value, and an `ok` on top of it;
- **`TimeStamp` is accepted and ignored.** The renderer shows it, which means a
  row that was read carries it back; and the server stamps its own clock on any
  write, so there is no honest way to take it into account anyway. To reject it
  would mean breaking the "read → corrected → wrote" loop the surface is there
  for;
- **`Expires` is accepted** - in the same RFC3339 it is shown in, or as a
  number - and is range-checked **on the write**: a moment beyond the edge of
  the calendar would land on disk and would make the row unshowable after every
  restart;
- a batch is assembled **in full before it is applied**: a row that did not
  parse cancels the whole call, and the table stays as it was. The same rule as
  for the streaming write.

`bulk_insert_or_replace_rows` goes through `BulkWriteMode::InsertOrReplace` -
one entry into the table and one event to the subscribers. A loop over
`insert_or_replace_row` would give N events and half of the state applied on a
disconnect, so both descriptions say to prefer the batch.

### Reading

- **a default window of 100 rows** and `has_more`. The JSON version has no
  window at all, and the answer to "what is in traders" is the whole table into
  the context. The "there is more" flag is computed by asking for one row more,
  not by a second pass for the sake of a count;
- **the last-read mark does not move.** `DbTable::get_rows` is taken, not
  `db_operations::read::get_rows`. Otherwise an agent paging through a table
  would save from eviction exactly those cold rows someone went to look at - the
  same argument `Row/Statistics` stands on. This diverges from `GET /api/Row`
  deliberately: there a human is looking at one row, here a machine is walking
  through them;
- **backup rows are shown through the schemas of the archive itself.** They sit
  in the table's attributes inside the zip, so a partition taken off a version
  of an entity that nobody runs any more is readable - while a live table with
  such a schema may no longer exist.

### What is not here

Creating a table, changing its attributes, deleting a table, deleting a
namespace, taking a backup, downloading or uploading one, `FlushToDisk`,
`MigrateFrom`. Exactly the same things that are not in the JSON version's MCP:
the set is that version's, and adding to it one at a time means giving yourself
a second list of what an agent is allowed to do.

`restore_backup` and `move_table_to_namespace` are there all the same, even
though both are operator's calls and have no HTTP routes of their own (see
"HTTP"). There is no contradiction here: the MCP is not a REST surface standing
next to them, but a separate one, closed behind the gate; the JSON version has
these two tools, and the gate is what makes an operator's call one that can be
handed to an agent.

## Reader

`Greeting(AppName, Version, NameSpace) → SessionId`; `Subscribe(SessionId,
Table)` **streams the snapshot back**; `GetChange` - a 5 s long poll, a FIFO
queue of sync chunks, **one chunk per answer**, everything empty = ping.

Batches are cut up (1000 records or 1 MB) and closed by an `End` of their own -
the reader accumulates and applies transactionally. An unfinished batch is not
applied at all. Since the reader accumulates until the `End` anyway, the cut is
allowed **inside** a partition too: one partition may arrive in two
`InitPartitions` chunks, and the reader has to glue them together rather than
let the second overwrite the first.

`CleanTable`, `DeleteTable` and `CleanPartitions` are instructions, not
records - they need no `End`.

**`Subscribe` to a table that does not exist is not an error.** The namespace is
resolved with `get_or_create` - this is the second place after creating a table
where resolution **creates** (over HTTP it does this nowhere): a subscription is
a claim on the future, and bringing what it names into existence is exactly its
job. The table may still not be there: a reader rolled out before its writer is
the ordinary cold start, not a failure. The subscription is registered all the
same, the answer is an empty stream, and the very first write reaches the reader
through the queue. Refusing here wedged the reader **whole**: the session starts
over from the greeting, and the list of what is already subscribed lives inside
the session - so no table standing in the queue behind an unknown one ever got
as far as being subscribed, and the incremental path never started at all. This
is the same rule by which a subscription survives a `DeleteTable`, only from the
other end of a table's life. The JSON version answers a cold start the same way,
and its comment says it outright: an Error contract crashes the reader inside
the SDK. The client for its part puts an empty snapshot in on `NotFound` - but
**only into a table nothing has arrived into yet**, because "there are no rows"
on top of a live cache is forbidden - and moves on to the next table without
killing the session.

**A session's queue is capped at 1000 chunks.** `GetChange` answers with one
chunk per answer, which means a reader that is keeping up clears a thousand of
them in a second of round trips, while one that is a thousand behind is not
catching up one chunk per answer. And it is not only about memory:
`SyncChunk::UpdateRows` holds cloned `Arc<DbRow>`s, so the queue keeps alive
rows the table itself has already released. The session TTL is no help here -
`GetChange` touches the session on every entry. So the queue is thrown away
whole and the session is forgotten on its next call: the reader gets `unknown
session` and does exactly the full re-subscription the failure model has anyway.
Half a picture is worse than none - which is why all of it is thrown away, and
nothing accumulates while the reader comes back. The depth is visible in
`Status`/`Connections` (`pendingChunks`).

**The failure model is TCP's:** any call error or `unknown session` = a full
re-subscription. Which is why there are neither packet numbers nor
acknowledgements.

**It rests on two things:** the subscription registration and the snapshot under
one taking of the write lock (`DbTable::register_and_snapshot`, with the proof
in its doc comment); the client's 15 s deadline against the 5-second polling.
The session TTL is 30 s.

## Operation → event to the reader

| operation | event to the reader |
|---|---|
| `Insert`, `InsertOrReplace`, `Replace`, `InsertOrReplaceIfNew` (a single row) | `UpdateRows` + `End` |
| `BulkWrite` / `InsertOrReplace`, `InsertOrReplaceIfNew` | `UpdateRows` + `End` |
| `BulkWrite` / `CleanPartitionsAndInsert` | `InitPartitions` + `End` |
| `BulkWrite` / `CleanTableAndInsert` | `CleanTable`, then `UpdateRows` + `End` |
| `DeleteRow`, `BulkDelete` | `DeleteRows` + `End` (only the keys actually deleted) |
| `DeletePartitions` | `CleanPartitions` (only the keys actually removed) |
| `CleanTable` | `CleanTable` (always, even if there was nothing to clean) |
| `SetTableAttributes`, and `CreateTableIfNotExists` which found something to change | `UpdateTableAttributes` |
| `DeleteTable` | `DeleteTable` |
| `CommitTransaction` | one instruction per action, in the transaction's order, as one unbroken series of chunks |
| GC (`Expires`, limits) | `CleanPartitions` and/or `DeleteRows` + `End` - as one unbroken series of chunks |

**A transaction is atomic on the server, but not in the reader's cache.**
Instructions are handed out one per answer, so the reader passes through the
transaction's intermediate states. The sign that "all of it has arrived" is its
last action: the queue keeps the order, so everything before it has already been
applied.

`DeleteTable` is the ninth type in `GetChangeGrpcResponse`, added together with
the operation: `CleanTable` would have said "the table is empty", not "the table
is gone". The subscription is **kept** - a table with the same name may be
created again, and the reader will start getting rows again without
re-subscribing.

`UpdateTableAttributes` is the tenth, and it exists because the reader's cache
is a `DbTable` from the core with attributes of its own. A snapshot carries
rows, not attributes, so until the first such event the reader shows the
defaults; after that it says the same thing the server does. The event goes out
only when the attributes really did change - hence the rule by which a
`CreateIfNotExists` repeated with the same numbers wakes nobody.

## UI

The HTTP port carries a web interface - a Dioxus SPA in `ui/`, built into a
committed `wwwroot/` and served by `StaticFilesMiddleware`, last of the
middleware. `index.html` is wired in both as the index and as the answer to a
miss: `/data` and `/snapshots` are the application's routes, not files, and a
404 there has to hand the browser the application, not an error.

**Ported from the JSON version, not written from scratch.** Its `ui/` is 7200
lines of finished interface for the same product, and rewriting it would have
meant getting a different UI for a task that said "the same one". Copied byte
for byte, and from there - cutting out and rewiring.

`ui/` is **not a workspace member** (`exclude`): it is built by `dx` for
`wasm32`, not by cargo for the host. `build-ui.sh` puts the result into
`wwwroot/`, `wwwroot/` is committed and baked into the image by the Dockerfile -
which means the server does not depend on whether `dx` is installed on the
machine it is built on.

**The UI's models are ours, not its.** `/api/Status` here answers the same
question with a different document: it is keyed by namespace, it has no
connected writers (a write is a unary call - between two writes there is nothing
to enumerate), and in their place it has open transactions. So
`StatusBarApiModel` and `WriterApiModel` are deleted outright, and the writers
table became a transactions table: a transaction that stopped receiving actions
is exactly what an operator ought to be looking at. Duplicating someone else's
shape next to ours would have been two truths about one thing.

Cut out along with the things that do not exist here: the row-compression toggle
(`Compressed` was cut from the server by decision - see "Parity with the JSON
version"), the traffic counters and the writers table. A zero in a tile is worse
than a missing tile: it reads as "there is no traffic", not as "that does not
happen here".

**The only thing the page keeps on the server is the two reader-health
thresholds** (`ui-settings.json` in the persistence root). That is a statement
about **this** server: whoever tuned them said what counts as slow here, and the
next person to open the page should see the same. Everything that is one
browser's preference lives in the browser. The file lies inside the persistence
root as a plain file - the walk over namespaces looks only at directories, so it
does not get in its way.


## Done

The core (parser, DbRow/Partition/Table/Instance, schemas in the table's
attributes) · persistence in full · Writer: `Ping`, `CreateTable(IfNotExists)`,
`GetTables`, `Insert`, `InsertOrReplace`, `InsertOrReplaceIfNew`, `Replace`,
`DeleteRow`, `BulkDelete`, `GetRow`, `GetRows`, `BulkWrite` (4 modes),
`CleanTable`, `DeleteTable`, `DeletePartitions`, transactions
(`Start`/`Post`/`Commit`/`Cancel`), GC (`Expires` on rows, eviction by both
limits and the handles to them), schema GC, TTL renewal on read,
`SetTableAttributes`, `HighestRowAndBelow`, `SinglePartitionMultipleRows`,
`GetTableSize`, `GetNamespaces`/`DeleteNamespace`/`MoveTableToNamespace`,
backups (a zip per namespace: take one, list, inspect, download, upload, restore
in full and per partition, the timer), `MigrateFrom` with the schema,
`FlushToDisk` · HTTP: reads rendered by the schema through `my-json`, writes
without an entity (rows, partitions, tables), `ApiKey`, `Status`, `Connections`,
`Row/Statistics`, Prometheus `/metrics` · MCP on `/mcp`: 15 tools and 2 prompts,
writes gated by a 10-minute window, JSON → protobuf by the table's schema ·
Reader in full · **the clients** (in the SDK): the writer covering the whole
Writer contract, the reader with a cache and re-subscription, macros - declaring
an entity and nested messages, a facade with the `macros` / `data-writer` /
`data-reader` features.

**336 tests** - 210 here and 126 in the SDK, `clippy -D warnings` and `fmt` are
clean. Thirty-one of the ones here bring up a real server on a socket and
exercise both clients, pulled in by tag, including a server crash,
re-subscription and a row written through MCP and read back by the client's
`from_slice`; plus the `round_trip` example in the SDK - against a server in a
separate process.

## Parity with the JSON version

Checked against the `MyNoSqlServer` sources (76 HTTP routes, gRPC,
`db_operations`, MCP). **Parity here is by operation, not by route**: no
operation is left unclosed, but its 76 HTTP addresses are not the list of what
has to be here. MCP is the exception: there parity is **by tool**, because a
tool is exactly the operation as the caller sees it (see "MCP").

In the JSON version HTTP is the only entry, so everything is exposed there. For
us it is a surface for the UI, while the operator's handles live on gRPC (see
"HTTP"). So a missing HTTP address is not a gap - the operation is there, it is
just where it is called from. And the response shapes do not have to match -
they answer to the UI.

The one item that stood in this list to the very end - **`Compressed`** - is
closed by a decision, not by work: keeping rows zstd-compressed in memory was
the JSON server's answer to JSON text. A protobuf row is already the compact
form, and compressing it a second time would mean paying decompression on every
lookup through the sorted vector (the keys are a `ContentRange` into `raw`, read
without copying; under compression they would have to be moved out into strings
of their own).

The attribute is meanwhile **cut out entirely** - from both proto contracts
(number 4 `reserved`, so that nobody reuses it), from `DbTableAttributes`, from
`tables.meta`, from HTTP and from every view. An attribute that is accepted,
stored and shown but that nobody reads is exactly the dead branch that has no
place here. Old `tables.meta` with a `Compressed:` key still read as before:
serde skips the unknown key, and there is a test for that - otherwise the server
would have lost a table because of a field it had itself stopped understanding.

## The known cost

The "always `End`" rule means two round-trips even for a single changed row.
Cured by an `IsLast` flag in the model instead of a separate `End` contract -
but that changes the agreement.

`DeleteTable` lands on disk in two stages: the persistence queue always hands
out a table's metadata before its partitions, so first the record in
`tables.meta` disappears, then the slots are freed. A crash between the two will
bring the table back with default attributes - the same class of loss as a crash
right after the call (the deletion just did not happen), and the start-up load
already says so out loud. Cured by a separate `PersistTask::DeleteTable` - not
doing it while this is the only user.
