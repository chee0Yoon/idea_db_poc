//! MCP stdio gateway. All graph mutations exposed to agents terminate here.

use std::collections::BTreeMap;

use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, ListResourcesResult, PaginatedRequestParams, ReadResourceRequestParams,
        ReadResourceResponse, ReadResourceResult, Resource, ResourceContents,
    },
    schemars,
    service::RequestContext,
    tool, tool_handler, tool_router, RoleServer, ServerHandler,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    backup, error::ApiError, ingest, mutation, neo4j::Neo4j, query, retry, staging, store,
};

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ParamsArgs {
    /// Documented query parameters. Unknown keys are rejected by the domain API.
    #[serde(default)]
    pub params: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RecordArgs {
    pub id: String,
    #[serde(default)]
    pub params: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct BodyArgs {
    /// Existing idea_db JSON request contract.
    pub body: Value,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct UploadArgs {
    pub upload_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ExportArgs {
    pub project_id: String,
    #[serde(default)]
    pub include_receipts: bool,
}

#[derive(Clone)]
pub struct IdeaDbMcp {
    neo: Neo4j,
    tool_router: ToolRouter<Self>,
}

impl IdeaDbMcp {
    pub fn new(neo: Neo4j) -> Self {
        Self {
            neo,
            tool_router: Self::tool_router(),
        }
    }

    async fn context(&self) -> Result<validate::Context, CallToolResult> {
        retry::transient(|| store::read_context(&self.neo))
            .await
            .map_err(domain_error)
    }
}

use crate::validate;

#[tool_router]
impl IdeaDbMcp {
    #[tool(
        description = "Read project state and records",
        annotations(
            title = "Read idea state",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn idea_state(&self, Parameters(args): Parameters<ParamsArgs>) -> CallToolResult {
        let ctx = match self.context().await {
            Ok(v) => v,
            Err(e) => return e,
        };
        result(query::state(&ctx, &args.params))
    }

    #[tool(
        description = "Read one immutable record",
        annotations(
            title = "Read idea record",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn idea_record(&self, Parameters(args): Parameters<RecordArgs>) -> CallToolResult {
        let ctx = match self.context().await {
            Ok(v) => v,
            Err(e) => return e,
        };
        result(query::record(&ctx, &args.id, &args.params))
    }

    #[tool(
        description = "List saved raw captures",
        annotations(
            title = "List idea captures",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn idea_captures(&self, Parameters(args): Parameters<ParamsArgs>) -> CallToolResult {
        let ctx = match self.context().await {
            Ok(v) => v,
            Err(e) => return e,
        };
        result(query::captures(&ctx, &args.params))
    }

    #[tool(
        description = "Read a bounded immutable composition snapshot",
        annotations(
            title = "Read idea snapshot",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn idea_snapshot(&self, Parameters(args): Parameters<ParamsArgs>) -> CallToolResult {
        let ctx = match self.context().await {
            Ok(v) => v,
            Err(e) => return e,
        };
        result(query::snapshot(&ctx, &args.params))
    }

    #[tool(
        description = "Search project history and reusable ideas",
        annotations(
            title = "Search ideas",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn idea_search(&self, Parameters(args): Parameters<BodyArgs>) -> CallToolResult {
        result(retry::transient(|| ingest::search(&self.neo, args.body.clone())).await)
    }

    #[tool(
        description = "Read goals and their current assessments",
        annotations(
            title = "Read idea goals",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn idea_goals(&self, Parameters(args): Parameters<ParamsArgs>) -> CallToolResult {
        let ctx = match self.context().await {
            Ok(v) => v,
            Err(e) => return e,
        };
        result(query::goals(&ctx, &args.params))
    }

    #[tool(
        description = "Read bounded revision occurrences",
        annotations(
            title = "Read idea occurrences",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn idea_occurrences(&self, Parameters(args): Parameters<ParamsArgs>) -> CallToolResult {
        let ctx = match self.context().await {
            Ok(v) => v,
            Err(e) => return e,
        };
        result(query::occurrences(&ctx, &args.params))
    }

    #[tool(
        description = "Export one project and optional idempotency receipts",
        annotations(
            title = "Export idea project",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn idea_export(&self, Parameters(args): Parameters<ExportArgs>) -> CallToolResult {
        result(
            retry::transient(|| {
                backup::export(
                    &self.neo,
                    Some(args.project_id.clone()),
                    args.include_receipts,
                )
            })
            .await,
        )
    }

    #[tool(
        description = "Persist a raw source capture before local atomization",
        annotations(
            title = "Create idea capture",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn idea_capture_create(&self, Parameters(args): Parameters<BodyArgs>) -> CallToolResult {
        result(
            retry::transient(|| mutation::create_capture(&self.neo, args.body.clone()))
                .await
                .map(|(_, value)| value),
        )
    }

    #[tool(
        description = "Validate a package without committing it",
        annotations(
            title = "Validate idea package",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn idea_package_validate(
        &self,
        Parameters(args): Parameters<BodyArgs>,
    ) -> CallToolResult {
        result(retry::transient(|| mutation::validate_only(&self.neo, args.body.clone())).await)
    }

    #[tool(
        description = "Atomically apply an immutable idea package",
        annotations(
            title = "Apply idea package",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn idea_package_apply(&self, Parameters(args): Parameters<BodyArgs>) -> CallToolResult {
        result(retry::transient(|| ingest::apply(&self.neo, args.body.clone())).await)
    }

    #[tool(
        description = "Replace one occurrence with expected-head conflict protection",
        annotations(
            title = "Replace idea occurrence",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn idea_occurrence_replace(
        &self,
        Parameters(args): Parameters<BodyArgs>,
    ) -> CallToolResult {
        result(retry::transient(|| ingest::replace(&self.neo, args.body.clone())).await)
    }

    #[tool(
        description = "Import a versioned idea_db export into an empty namespace",
        annotations(
            title = "Import idea project",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn idea_import(&self, Parameters(args): Parameters<BodyArgs>) -> CallToolResult {
        result(retry::transient(|| backup::import(&self.neo, args.body.clone())).await)
    }

    #[tool(
        description = "Stage and validate a locally atomized package; returns upload_id and prepared_digest without raw vectors",
        annotations(
            title = "Preview idea upload",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn idea_upload_preview(&self, Parameters(args): Parameters<BodyArgs>) -> CallToolResult {
        result(retry::transient(|| ingest::preview(&self.neo, args.body.clone())).await)
    }

    #[tool(
        description = "Apply a staged upload using only {upload_id, prepared_digest}",
        annotations(
            title = "Apply idea upload",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn idea_upload_apply(&self, Parameters(args): Parameters<BodyArgs>) -> CallToolResult {
        result(retry::transient(|| ingest::apply(&self.neo, args.body.clone())).await)
    }

    #[tool(
        description = "Withdraw an abandoned staged upload while retaining its audit history",
        annotations(
            title = "Discard idea upload",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn idea_upload_discard(
        &self,
        Parameters(args): Parameters<UploadArgs>,
    ) -> CallToolResult {
        result(retry::transient(|| staging::discard(&self.neo, &args.upload_id)).await)
    }
}

#[tool_handler(
    router = self.tool_router,
    name = "idea-db",
    version = "0.1.0",
    instructions = "Use this closed-world gateway for all idea_db reads and writes. Atomize source documents locally before idea_upload_preview; never invent raw Cypher."
)]
impl ServerHandler for IdeaDbMcp {
    fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListResourcesResult, rmcp::ErrorData>> + Send + '_
    {
        std::future::ready(Ok(ListResourcesResult::with_all_items(vec![
            Resource::new("idea-db://contracts/mcp-intake", "mcp-intake")
                .with_title("Local AI intake contract")
                .with_description("Required local decomposition and staged upload workflow")
                .with_mime_type("text/markdown"),
            Resource::new("idea-db://contracts/domain-api", "domain-api")
                .with_title("idea_db domain contract")
                .with_description("Immutable graph package and query contract")
                .with_mime_type("text/markdown"),
        ])))
    }

    fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<ReadResourceResponse, rmcp::ErrorData>> + Send + '_
    {
        let value = match request.uri.as_str() {
            "idea-db://contracts/mcp-intake" => Ok(include_str!("../docs/mcp-intake.md")),
            "idea-db://contracts/domain-api" => Ok(include_str!("../docs/api.md")),
            _ => Err(rmcp::ErrorData::invalid_params(
                "unknown idea_db resource URI",
                None,
            )),
        };
        std::future::ready(value.map(|text| {
            ReadResourceResult::new(vec![ResourceContents::text(text, request.uri)]).into()
        }))
    }
}

fn result(value: crate::error::ApiResult<Value>) -> CallToolResult {
    match value {
        Ok(value) => CallToolResult::structured(value),
        Err(error) => domain_error(error),
    }
}

fn domain_error(error: ApiError) -> CallToolResult {
    CallToolResult::structured_error(json!({
        "error": {
            "status": error.status.as_u16(),
            "code": error.code,
            "message": error.message,
            "details": error.details,
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use rmcp::ServiceExt;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    fn service() -> IdeaDbMcp {
        let config = Config {
            bind: String::new(),
            neo4j_uri: "http://127.0.0.1:9".into(),
            neo4j_user: "neo4j".into(),
            neo4j_password: None,
            neo4j_database: "neo4j".into(),
            token: None,
            static_dir: String::new(),
        };
        IdeaDbMcp::new(Neo4j::new(&config).unwrap())
    }

    async fn request(
        writer: &mut tokio::io::WriteHalf<tokio::io::DuplexStream>,
        reader: &mut BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>,
        value: Value,
    ) -> Value {
        writer
            .write_all(value.to_string().as_bytes())
            .await
            .unwrap();
        writer.write_all(b"\n").await.unwrap();
        writer.flush().await.unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        serde_json::from_str(&line).unwrap()
    }

    #[tokio::test]
    async fn stdio_initialize_list_error_and_eof_shutdown() {
        let (client, server) = tokio::io::duplex(128 * 1024);
        let (server_read, server_write) = tokio::io::split(server);
        let serve_task =
            tokio::spawn(
                async move { service().serve((server_read, server_write)).await.unwrap() },
            );
        let (client_read, mut client_write) = tokio::io::split(client);
        let mut client_read = BufReader::new(client_read);

        let initialized = request(
            &mut client_write,
            &mut client_read,
            json!({
                "jsonrpc":"2.0","id":1,"method":"initialize","params":{
                    "protocolVersion":"2025-11-25","capabilities":{},
                    "clientInfo":{"name":"test","version":"1"}
                }
            }),
        )
        .await;
        assert_eq!(initialized["result"]["protocolVersion"], "2025-11-25");
        let running = serve_task.await.unwrap();
        client_write
            .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
            .await
            .unwrap();

        let listed = request(
            &mut client_write,
            &mut client_read,
            json!({
                "jsonrpc":"2.0","id":2,"method":"tools/list","params":{}
            }),
        )
        .await;
        let tools = listed["result"]["tools"].as_array().unwrap();
        assert!(tools
            .iter()
            .any(|tool| tool["name"] == "idea_upload_preview"));
        assert!(tools
            .iter()
            .any(|tool| tool["name"] == "idea_upload_discard"));
        let import = tools
            .iter()
            .find(|tool| tool["name"] == "idea_import")
            .unwrap();
        assert_eq!(import["annotations"]["destructiveHint"], true);
        assert_eq!(import["annotations"]["openWorldHint"], false);

        let resources = request(
            &mut client_write,
            &mut client_read,
            json!({"jsonrpc":"2.0","id":4,"method":"resources/list","params":{}}),
        )
        .await;
        assert!(resources["result"]["resources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|resource| resource["uri"] == "idea-db://contracts/mcp-intake"));
        let intake = request(
            &mut client_write,
            &mut client_read,
            json!({"jsonrpc":"2.0","id":5,"method":"resources/read","params":{"uri":"idea-db://contracts/mcp-intake"}}),
        )
        .await;
        assert!(intake["result"]["contents"][0]["text"]
            .as_str()
            .unwrap()
            .contains("local AI client"));

        let unknown = request(
            &mut client_write,
            &mut client_read,
            json!({
                "jsonrpc":"2.0","id":3,"method":"tools/call",
                "params":{"name":"raw_cypher","arguments":{}}
            }),
        )
        .await;
        assert!(unknown.get("error").is_some());

        drop(client_write);
        drop(client_read);
        let ended =
            tokio::time::timeout(std::time::Duration::from_secs(5), running.waiting()).await;
        assert!(ended.is_ok(), "server must stop when stdin reaches EOF");
    }

    fn modern_meta() -> Value {
        json!({
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientInfo": {"name":"stateless-test","version":"1"},
            "io.modelcontextprotocol/clientCapabilities": {}
        })
    }

    #[tokio::test]
    async fn stateless_discover_lists_tools_and_reads_resources() {
        let (client, server) = tokio::io::duplex(128 * 1024);
        let (server_read, server_write) = tokio::io::split(server);
        let serve_task =
            tokio::spawn(
                async move { service().serve((server_read, server_write)).await.unwrap() },
            );
        let (client_read, mut client_write) = tokio::io::split(client);
        let mut client_read = BufReader::new(client_read);

        let discovered = request(
            &mut client_write,
            &mut client_read,
            json!({"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":modern_meta()}}),
        )
        .await;
        let versions = discovered["result"]["supportedVersions"]
            .as_array()
            .unwrap();
        assert!(versions.iter().any(|version| version == "2026-07-28"));
        let running = serve_task.await.unwrap();

        let listed = request(
            &mut client_write,
            &mut client_read,
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{"_meta":modern_meta()}}),
        )
        .await;
        assert!(listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "idea_search"));

        let resources = request(
            &mut client_write,
            &mut client_read,
            json!({"jsonrpc":"2.0","id":3,"method":"resources/list","params":{"_meta":modern_meta()}}),
        )
        .await;
        assert_eq!(
            resources["result"]["resources"].as_array().unwrap().len(),
            2
        );

        let intake = request(
            &mut client_write,
            &mut client_read,
            json!({"jsonrpc":"2.0","id":4,"method":"resources/read","params":{"uri":"idea-db://contracts/mcp-intake","_meta":modern_meta()}}),
        )
        .await;
        assert!(intake["result"]["contents"][0]["text"]
            .as_str()
            .unwrap()
            .contains("local AI client"));

        let unknown = request(
            &mut client_write,
            &mut client_read,
            json!({"jsonrpc":"2.0","id":5,"method":"idea-db/raw-cypher","params":{"_meta":modern_meta()}}),
        )
        .await;
        assert_eq!(unknown["error"]["code"], -32601);

        drop(client_write);
        drop(client_read);
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(5), running.waiting())
                .await
                .is_ok()
        );
    }
}
