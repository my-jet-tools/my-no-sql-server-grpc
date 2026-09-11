use std::collections::HashMap;

use mcp_server_middleware::*;

pub struct McpWritesEnablePolicyPromptHandler;

impl PromptDefinition for McpWritesEnablePolicyPromptHandler {
    const PROMPT_NAME: &'static str = "mcp_writes_enable_policy";

    const DESCRIPTION: &'static str = "How the write tools of this server are gated: a person opens a 10-minute window over \
         HTTP, and there is no tool which opens it. Read before any write call.";

    fn get_argument_descriptions() -> Vec<PromptArgumentDescription> {
        Vec::new()
    }
}

#[async_trait::async_trait]
impl McpPromptService for McpWritesEnablePolicyPromptHandler {
    async fn execute_prompt(
        &self,
        _model: &HashMap<String, String>,
    ) -> Result<PromptExecutionResult, String> {
        let body = r#"# MCP writes: how they are opened

Every tool which changes anything — `insert_or_replace_row`,
`bulk_insert_or_replace_rows`, `delete_row`, `bulk_delete_rows`,
`delete_partitions`, `clean_table`, `move_table_to_namespace`,
`restore_backup` — is SHUT by default.

There is no password and, deliberately, **no tool which opens them**. A person
opens the window from outside this surface:

> `POST /api/Mcp/Writes?enabled=true` on the server's HTTP port
> (`curl -X POST 'http://<host>:5123/api/Mcp/Writes?enabled=true'`, plus an
> `apikey:` header if the server was given one).

Once open, writes stay open for **10 minutes** and then shut themselves.
`POST /api/Mcp/Writes?enabled=false` shuts them at once, and a server restart
always comes up shut. How much of the window is left is in `GET /api/Status`
under `mcpWrites`.

## Rules

1. **Never assume writes are open.** The read tools (`get_namespaces`,
   `get_list_of_tables`, `get_rows`, and the backup ones) always work.
2. **A write refused as SHUT is not a retry.** Tell the user the exact call
   above, then wait for them to say they have made it. Repeating the tool in a
   loop only wastes the turn.
3. **The window can lapse mid-task.** If a later write fails after earlier ones
   went through, the ten minutes are up — ask for it to be opened again, and say
   which writes already landed.
4. **The window is permission, not review.** It says an agent may write; it does
   not say *what*. Before a wide or destructive call — `clean_table`, a
   `bulk_delete_rows` spanning partitions, `delete_partitions`,
   `restore_backup` — put the list, or the counts, in the reply and let the user
   confirm it. Nothing here is undoable: there is no bin, and a subscriber's
   cache follows the server rather than the other way round.
5. **Prefer one call to a loop.** `bulk_delete_rows` takes every partition at
   once and `bulk_insert_or_replace_rows` every row; each of them enters the
   table once, so subscribers see one change rather than N — and half a loop
   interrupted is a state nobody asked for."#;

        Ok(PromptExecutionResult {
            description: "How MCP writes are opened (HTTP call, 10-minute window).".to_string(),
            message: body.to_string(),
        })
    }
}
