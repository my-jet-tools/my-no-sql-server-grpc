use std::collections::HashMap;

use mcp_server_middleware::*;

/// The prompt which replaces the JSON version's `paste_delete_via_ui`.
///
/// That one exists because its `bulk_delete_rows` takes a single partition, so a
/// wide delete had to be handed to a UI dialog to be done in one go. Here the
/// same tool takes every partition at once, and there is no UI to hand anything
/// to - so the workflow it describes has nothing to be. What a caller of *this*
/// server does need told is the thing the JSON version never has to say: rows
/// are protobuf, and JSON only reaches them through a schema.
pub struct EntitySchemaPolicyPromptHandler;

impl PromptDefinition for EntitySchemaPolicyPromptHandler {
    const PROMPT_NAME: &'static str = "entity_schema_policy";

    const DESCRIPTION: &'static str = "How rows of this server become JSON and back: they are protobuf, shown and written \
         through the schema their own client registered. Read before writing a row, and when a \
         row comes back with numbers for field names.";

    fn get_argument_descriptions() -> Vec<PromptArgumentDescription> {
        Vec::new()
    }
}

#[async_trait::async_trait]
impl McpPromptService for EntitySchemaPolicyPromptHandler {
    async fn execute_prompt(
        &self,
        _model: &HashMap<String, String>,
    ) -> Result<PromptExecutionResult, String> {
        let body = r#"# Rows here are protobuf, not JSON

This is MyNoSqlServer with **protobuf** entities. A stored row is a protobuf
message; the JSON you see and the JSON you send are a view of it, and the view
needs a **schema**.

A schema is registered by the service which owns the entity: it travels with
every write on the gRPC side, under an id that service folded out of its own
type. This server never invents one. So:

## Reading

`get_rows` renders each row through the schema **that row** was written under —
old and new versions of an entity sit side by side in one table and each is
shown as it was written.

A row shown with **numbers for field names** (`"5": 12.5`) means this server has
no schema for it: nobody has written that version through this server since it
came up, or the stored schema does not read back. The values are still right;
only the names are missing.

## Writing

`insert_or_replace_row` and `bulk_insert_or_replace_rows` take JSON and turn it
into protobuf through a schema **the table already holds**. That means:

- **A table nobody has ever written to can not be written to from here.** There
  is no shape to build a row with. The owning service has to write once first —
  that write is what teaches this server the shape.
- **A field name the schema does not have is refused**, and the answer lists the
  names it does have. A misspelled field is never silently dropped.
- **`PartitionKey` and `RowKey` are ordinary fields** of the schema and are
  required — a row without them is not a row.
- **`TimeStamp` is ignored.** The server stamps its own clock on every write, so
  a row read out of `get_rows` can be handed straight back without editing it.
- **`Expires` is honoured**: RFC3339 as `get_rows` shows it, or `null` for
  never. It is what the garbage collector reads.
- **`schema_id` is only needed when a table holds more than one version.**
  `get_list_of_tables` reports `schemas_count`; when it is 1 — which it is for
  almost every table — leave `schema_id` out. When it is 2 or more a deploy is
  in flight, or two entities are aimed at one table, and the call refuses rather
  than guessing which version your row is.

## What this surface can not do

It can not create a table, change table attributes, delete a table, or migrate
one. Those are operator calls and they live on the gRPC contract, which is not
exposed here."#;

        Ok(PromptExecutionResult {
            description: "How protobuf rows are shown as JSON and written back through a schema."
                .to_string(),
            message: body.to_string(),
        })
    }
}
