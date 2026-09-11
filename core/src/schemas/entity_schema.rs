/// The shape a stored entity was written with, as it arrives and as it is kept.
///
/// The server does not need it to store or serve a row - the four reserved
/// fields are found by walking the wire format. It needs it to *show* a row:
/// turning stored bytes into readable JSON is impossible without field names and
/// types. Every row remembers the id of the schema it was written with, so an
/// entity that changed shape does not make the rows written before it
/// unreadable.
#[derive(Clone)]
pub struct EntitySchema {
    /// A constant the client's macro folded out of the type at compile time, and
    /// which this server never recomputes - see
    /// [`crate::schemas::schema_hash_begin`] for the rule and who owns it. What
    /// the server does check is that an id it already knows arrives with the
    /// same bytes it knew it by.
    pub id: u64,
    /// The canonical bytes of a [`crate::schemas::Schema`]. There is no root
    /// message name beside them: a schema names its own root.
    pub schema: Vec<u8>,
}

impl EntitySchema {
    pub fn new(id: u64, schema: Vec<u8>) -> Self {
        Self { id, schema }
    }
}
