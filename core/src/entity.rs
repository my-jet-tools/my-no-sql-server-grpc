use crate::db_entity::DbEntityParseFail;

/// Everything the clients need of an application's entity.
///
/// Nobody writes this by hand: the `my_no_sql_entity` macro rewrites the struct
/// it is put on and implements this over it. It is here, in the core, because
/// both clients mean the same thing by it - an application should not have to
/// describe its entity twice to write it and to read it.
///
/// Serialization belongs here rather than to `prost` because the four reserved
/// fields are this server's contract, not protobuf's: the macro knows their
/// numbers, writes them itself, and an application never gets the chance to put
/// something else at 1 or 3.
pub trait MyNoSqlEntity: Sized {
    const TABLE_NAME: &'static str;

    /// The number every row of this entity is stored under, folded out of the
    /// type at **compile time** - the type's own fields, plus the `SCHEMA_ID` of
    /// every message they carry. See [`crate::schemas::schema_hash_begin`].
    ///
    /// A constant rather than a hash of the built schema because it is the one
    /// thing the two sides must agree on without talking: the server never
    /// recomputes it, so there is one hash, on one side, with nothing to drift.
    const SCHEMA_ID: u64;

    /// Built once per type, not once per write, and travelling with every row so
    /// a server which has never seen this version of the entity can still show
    /// its rows.
    fn get_schema() -> &'static MyNoSqlEntitySchema;

    fn get_partition_key(&self) -> &str;
    fn get_row_key(&self) -> &str;

    /// Appends the entity in wire form. Proto3 leaves a default value off the
    /// wire, and so does this - which is also how `TimeStamp` stays absent until
    /// somebody sets it, letting the server stamp its own.
    fn serialize(&self, dest: &mut Vec<u8>);

    fn from_slice(src: &[u8]) -> Result<Self, DbEntityParseFail>;

    fn to_vec(&self) -> Vec<u8> {
        let mut result = Vec::new();
        self.serialize(&mut result);
        result
    }
}

/// A message an entity's field carries.
///
/// It is not an entity: it has no keys, no table and no schema of its own. What
/// it has is a name and a shape, so the entity using it can declare it in its
/// own schema - which is what lets the server render a nested value as a nested
/// object instead of a blob.
///
/// Like [`MyNoSqlEntity`], nobody writes this by hand: the `my_no_sql_message`
/// macro does.
pub trait MyNoSqlMessage: Sized + Default {
    /// The name it is declared under. A schema holds one message per name, and
    /// nothing qualifies it: two entities carrying the same message are two
    /// schemas, each with its own copy of it.
    const MESSAGE_NAME: &'static str;

    /// The same constant an entity has, by the same rule - which is what makes
    /// the entity's id a Merkle hash: a change in here reaches everything that
    /// carries this message.
    ///
    /// A constant can not name itself, so a message which carries itself, or two
    /// which carry each other, do not compile. That is the deliberate cost of
    /// having the id at compile time: a row of a NoSQL table is a flat record,
    /// and a tree is modelled with a second table or with `Vec<u8>`.
    const SCHEMA_ID: u64;

    /// Declares this message, and everything it carries in turn, into the schema
    /// being built.
    ///
    /// The generated body asks `has_message` first and returns the builder
    /// untouched if the name is already there, so the same message reached
    /// through two fields is one declaration.
    fn declare(builder: crate::schemas::SchemaBuilder) -> crate::schemas::SchemaBuilder;

    fn serialize(&self, dest: &mut Vec<u8>);

    fn from_slice(src: &[u8]) -> Result<Self, DbEntityParseFail>;

    fn to_vec(&self) -> Vec<u8> {
        let mut result = Vec::new();
        self.serialize(&mut result);
        result
    }
}

/// What travels with every write so a stored row can be shown under its own
/// field names.
pub struct MyNoSqlEntitySchema {
    pub id: u64,
    /// The canonical bytes of a [`crate::schemas::Schema`]. The id is not
    /// derived from them - it is [`MyNoSqlEntity::SCHEMA_ID`], and these are what
    /// the server compares when it is handed that id a second time.
    pub schema: Vec<u8>,
}

impl MyNoSqlEntitySchema {
    pub fn new(id: u64, schema: Vec<u8>) -> Self {
        Self { id, schema }
    }
}
