//! MCP transport and protocol integration.
//!
//! This module owns JSON-RPC protocol behavior and exposes the Codecks project operations without
//! coupling protocol clients to Codecks API transport details. The nested stdio module owns process
//! transport and lifecycle behavior.

use std::fmt;

use serde_json::{Map, Value, json};

use crate::codecks_api::CodecksClient;
use crate::domain::Project;
use crate::error::ApplicationError;
use crate::project_resolver::resolve_project;

mod stdio;

pub use stdio::run_stdio;

/// The latest MCP protocol revision implemented by the server.
pub const CURRENT_PROTOCOL_VERSION: &str = "2026-07-28";

const DEFAULT_LEGACY_PROTOCOL_VERSION: &str = "2025-11-25";
const LEGACY_PROTOCOL_VERSIONS: [&str; 4] =
    ["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];
const SERVER_NAME: &str = "codecks-mcp";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
const DISCOVERY_TTL_MILLISECONDS: u64 = 3_600_000;

/// A structured, credential-safe error exposed to MCP clients.
///
/// The code and message are static values derived from ApplicationError. Raw credentials, request
/// headers, response bodies, and upstream error text cannot be attached to this type.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct McpError {
    code: &'static str,
    message: &'static str,
}

impl McpError {
    /// Returns the stable machine-readable application error code.
    pub const fn code(self) -> &'static str {
        self.code
    }

    /// Returns the credential-safe message intended for an MCP client.
    pub const fn message(self) -> &'static str {
        self.message
    }
}

impl From<ApplicationError> for McpError {
    fn from(error: ApplicationError) -> Self {
        Self {
            code: error.code(),
            message: error.message(),
        }
    }
}

impl fmt::Display for McpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

/// A Codecks-backed MCP server that processes JSON-RPC requests asynchronously.
#[derive(Clone)]
pub struct McpServer {
    client: CodecksClient,
    legacy_initialized: bool,
}

impl McpServer {
    /// Creates an MCP server backed by an authenticated Codecks client.
    #[must_use]
    pub const fn new(client: CodecksClient) -> Self {
        Self {
            client,
            legacy_initialized: false,
        }
    }

    async fn handle_message(&mut self, message: Value) -> Option<Value> {
        let Some(request) = message.as_object() else {
            return Some(error_response(
                Value::Null,
                RpcError::invalid_request("JSON-RPC messages must be objects."),
            ));
        };
        let id = request.get("id").cloned();
        if id
            .as_ref()
            .is_some_and(|id| RequestKey::from_value(id).is_none())
        {
            return Some(error_response(
                Value::Null,
                RpcError::invalid_request("JSON-RPC request IDs must be strings or numbers."),
            ));
        }
        let response_id = id.clone().unwrap_or(Value::Null);

        if request.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Some(error_response(
                response_id,
                RpcError::invalid_request("The jsonrpc field must be \"2.0\"."),
            ));
        }
        let Some(method) = request.get("method").and_then(Value::as_str) else {
            return Some(error_response(
                response_id,
                RpcError::invalid_request("JSON-RPC requests require a method."),
            ));
        };
        let params = request.get("params");
        let id = id?;

        Some(match self.handle_request(method, params).await {
            Ok(result) => success_response(id, result),
            Err(error) => error_response(id, error),
        })
    }

    async fn handle_request(
        &mut self,
        method: &str,
        params: Option<&Value>,
    ) -> Result<Value, RpcError> {
        match method {
            "initialize" => self.initialize(params),
            "server/discover" => self.discover(params),
            "ping" => self.ping(params),
            "tools/list" => self.list_tools(params),
            "tools/call" => self.call_tool(params).await,
            _ => {
                self.protocol_era(params)?;
                Err(RpcError::method_not_found())
            }
        }
    }

    fn initialize(&mut self, params: Option<&Value>) -> Result<Value, RpcError> {
        let params = required_object(params)?;
        let requested_version = params
            .get("protocolVersion")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::invalid_params("protocolVersion must be a string."))?;
        require_object_field(params, "capabilities")?;
        require_object_field(params, "clientInfo")?;

        let protocol_version = if LEGACY_PROTOCOL_VERSIONS.contains(&requested_version) {
            requested_version
        } else {
            DEFAULT_LEGACY_PROTOCOL_VERSION
        };
        self.legacy_initialized = true;

        Ok(json!({
            "protocolVersion": protocol_version,
            "capabilities": server_capabilities(),
            "serverInfo": server_info(),
        }))
    }

    fn discover(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        validate_current_metadata(params)?;

        Ok(current_result(json!({
            "supportedVersions": [CURRENT_PROTOCOL_VERSION],
            "capabilities": server_capabilities(),
            "ttlMs": DISCOVERY_TTL_MILLISECONDS,
            "cacheScope": "public",
        })))
    }

    fn ping(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let era = self.protocol_era(params)?;
        Ok(complete_result(json!({}), era))
    }

    fn list_tools(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let era = self.protocol_era(params)?;
        if let Some(params) = params {
            let params = params
                .as_object()
                .ok_or_else(|| RpcError::invalid_params("params must be an object."))?;
            if params.contains_key("cursor") {
                return Err(RpcError::invalid_params(
                    "This server does not issue or accept pagination cursors.",
                ));
            }
            reject_unknown_fields(params, &["_meta"])?;
        }

        let mut result = json!({"tools": tool_definitions()});
        if matches!(era, ProtocolEra::Current) {
            let result = result
                .as_object_mut()
                .expect("internal MCP tool-list results must be objects");
            result.insert("ttlMs".to_owned(), json!(DISCOVERY_TTL_MILLISECONDS));
            result.insert("cacheScope".to_owned(), json!("public"));
        }

        Ok(complete_result(result, era))
    }

    async fn call_tool(&self, params: Option<&Value>) -> Result<Value, RpcError> {
        let era = self.protocol_era(params)?;
        let params = required_object(params)?;
        reject_unknown_fields(params, &["name", "arguments", "_meta"])?;
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::invalid_params("name must be a string."))?;
        let arguments = match params.get("arguments") {
            Some(arguments) => arguments
                .as_object()
                .cloned()
                .ok_or_else(|| RpcError::invalid_params("arguments must be an object."))?,
            None => Map::new(),
        };

        let result = match name {
            "list_projects" => {
                reject_unknown_fields(&arguments, &[])?;
                self.list_projects().await
            }
            "get_project" => {
                reject_unknown_fields(&arguments, &["project"])?;
                let selector = optional_string(&arguments, "project")?;
                self.get_project(selector).await
            }
            _ => return Err(RpcError::invalid_params("Unknown tool name.")),
        };

        Ok(match result {
            Ok(structured_content) => tool_result(structured_content, false, era),
            Err(error) => tool_result(error_content(error), true, era),
        })
    }

    fn protocol_era(&self, params: Option<&Value>) -> Result<ProtocolEra, RpcError> {
        let metadata = params
            .and_then(Value::as_object)
            .and_then(|params| params.get("_meta"))
            .map(|metadata| {
                metadata
                    .as_object()
                    .ok_or_else(|| RpcError::invalid_params("_meta must be an object."))
            })
            .transpose()?;
        let advertises_current_protocol = metadata.is_some_and(|metadata| {
            metadata.contains_key("io.modelcontextprotocol/protocolVersion")
        });

        if advertises_current_protocol {
            validate_current_metadata(params)?;
            Ok(ProtocolEra::Current)
        } else if self.legacy_initialized {
            Ok(ProtocolEra::Legacy)
        } else if metadata.is_some() {
            validate_current_metadata(params)?;
            Ok(ProtocolEra::Current)
        } else {
            Err(RpcError::server_not_initialized())
        }
    }

    async fn list_projects(&self) -> Result<Value, ApplicationError> {
        let projects = self.client.list_projects().await?;
        Ok(json!({
            "projects": projects.iter().map(project_value).collect::<Vec<_>>()
        }))
    }

    async fn get_project(&self, selector: Option<&str>) -> Result<Value, ApplicationError> {
        let projects = self.client.list_projects().await?;
        let project = resolve_project(&projects, selector)?;
        Ok(json!({"project": project_value(project)}))
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum RequestKey {
    String(String),
    Number(String),
}

impl RequestKey {
    fn from_value(value: &Value) -> Option<Self> {
        match value {
            Value::String(value) => Some(Self::String(value.clone())),
            Value::Number(value) => Some(Self::Number(value.to_string())),
            _ => None,
        }
    }
}

#[derive(Clone, Copy)]
enum ProtocolEra {
    Current,
    Legacy,
}

#[derive(Clone)]
struct RpcError {
    code: i64,
    message: &'static str,
    data: Option<Value>,
}

impl RpcError {
    const fn parse_error() -> Self {
        Self {
            code: -32700,
            message: "Parse error.",
            data: None,
        }
    }

    const fn invalid_request(message: &'static str) -> Self {
        Self {
            code: -32600,
            message,
            data: None,
        }
    }

    const fn method_not_found() -> Self {
        Self {
            code: -32601,
            message: "Method not found.",
            data: None,
        }
    }

    const fn invalid_params(message: &'static str) -> Self {
        Self {
            code: -32602,
            message,
            data: None,
        }
    }

    const fn server_not_initialized() -> Self {
        Self {
            code: -32002,
            message: "The server has not been initialized.",
            data: None,
        }
    }

    const fn too_many_requests() -> Self {
        Self {
            code: -32000,
            message: "Too many tool requests are already in flight.",
            data: None,
        }
    }

    fn unsupported_protocol_version(requested: &str) -> Self {
        Self {
            code: -32022,
            message: "Unsupported protocol version.",
            data: Some(json!({
                "supported": [CURRENT_PROTOCOL_VERSION],
                "requested": requested,
            })),
        }
    }
}

fn success_response(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error_response(id: Value, error: RpcError) -> Value {
    let mut response = json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": error.code,
            "message": error.message,
        }
    });
    if let Some(data) = error.data {
        response["error"]["data"] = data;
    }
    response
}

fn required_object(params: Option<&Value>) -> Result<&Map<String, Value>, RpcError> {
    params
        .and_then(Value::as_object)
        .ok_or_else(|| RpcError::invalid_params("params must be an object."))
}

fn require_object_field(params: &Map<String, Value>, field: &str) -> Result<(), RpcError> {
    params
        .get(field)
        .and_then(Value::as_object)
        .map(|_| ())
        .ok_or_else(|| RpcError::invalid_params("A required object field is missing or invalid."))
}

fn reject_unknown_fields(
    object: &Map<String, Value>,
    accepted_fields: &[&str],
) -> Result<(), RpcError> {
    if object
        .keys()
        .any(|field| !accepted_fields.contains(&field.as_str()))
    {
        Err(RpcError::invalid_params(
            "The request contains an unsupported field.",
        ))
    } else {
        Ok(())
    }
}

fn optional_string<'value>(
    object: &'value Map<String, Value>,
    field: &str,
) -> Result<Option<&'value str>, RpcError> {
    object
        .get(field)
        .map(|value| {
            value
                .as_str()
                .ok_or_else(|| RpcError::invalid_params("The project selector must be a string."))
        })
        .transpose()
}

fn validate_current_metadata(params: Option<&Value>) -> Result<(), RpcError> {
    let params = required_object(params)?;
    let metadata = params
        .get("_meta")
        .and_then(Value::as_object)
        .ok_or_else(|| RpcError::invalid_params("_meta must be an object."))?;
    let protocol_version = metadata
        .get("io.modelcontextprotocol/protocolVersion")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("Current protocol metadata requires a version."))?;

    if protocol_version != CURRENT_PROTOCOL_VERSION {
        return Err(RpcError::unsupported_protocol_version(protocol_version));
    }
    metadata
        .get("io.modelcontextprotocol/clientCapabilities")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            RpcError::invalid_params("Current protocol metadata requires client capabilities.")
        })?;

    Ok(())
}

fn server_info() -> Value {
    json!({
        "name": SERVER_NAME,
        "version": SERVER_VERSION,
    })
}

fn server_capabilities() -> Value {
    json!({
        "tools": {
            "listChanged": false,
        }
    })
}

fn current_result(mut result: Value) -> Value {
    let object = result
        .as_object_mut()
        .expect("internal MCP result values must be objects");
    object.insert("resultType".to_owned(), json!("complete"));
    object.insert(
        "_meta".to_owned(),
        json!({"io.modelcontextprotocol/serverInfo": server_info()}),
    );
    result
}

fn complete_result(result: Value, era: ProtocolEra) -> Value {
    match era {
        ProtocolEra::Current => current_result(result),
        ProtocolEra::Legacy => result,
    }
}

fn tool_result(structured_content: Value, is_error: bool, era: ProtocolEra) -> Value {
    let text = serde_json::to_string(&structured_content)
        .expect("internal MCP structured content must serialize");
    complete_result(
        json!({
            "content": [{
                "type": "text",
                "text": text,
            }],
            "structuredContent": structured_content,
            "isError": is_error,
        }),
        era,
    )
}

fn error_content(error: ApplicationError) -> Value {
    let error = McpError::from(error);
    json!({
        "error": {
            "code": error.code(),
            "message": error.message(),
        }
    })
}

fn project_value(project: &Project) -> Value {
    json!({
        "uuid": project.uuid(),
        "name": project.name(),
    })
}

fn tool_definitions() -> Value {
    json!([
        {
            "name": "list_projects",
            "title": "List Codecks projects",
            "description": "Lists every active Codecks project visible to the authenticated account.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false,
            },
            "outputSchema": project_result_schema(true),
            "annotations": {
                "readOnlyHint": true,
                "destructiveHint": false,
                "idempotentHint": true,
                "openWorldHint": true,
            },
        },
        {
            "name": "get_project",
            "title": "Get a Codecks project",
            "description": "Gets a Codecks project by UUID or exact name, or selects the only available project.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": {
                        "type": "string",
                        "description": "Optional project UUID or exact project name.",
                    }
                },
                "additionalProperties": false,
            },
            "outputSchema": project_result_schema(false),
            "annotations": {
                "readOnlyHint": true,
                "destructiveHint": false,
                "idempotentHint": true,
                "openWorldHint": true,
            },
        }
    ])
}

fn project_result_schema(is_list: bool) -> Value {
    let success_schema = if is_list {
        json!({
            "type": "object",
            "properties": {
                "projects": {
                    "type": "array",
                    "items": project_schema(),
                }
            },
            "required": ["projects"],
            "additionalProperties": false,
        })
    } else {
        json!({
            "type": "object",
            "properties": {
                "project": project_schema(),
            },
            "required": ["project"],
            "additionalProperties": false,
        })
    };

    json!({
        "type": "object",
        "oneOf": [
            success_schema,
            {
                "type": "object",
                "properties": {
                    "error": {
                        "type": "object",
                        "properties": {
                            "code": {"type": "string"},
                            "message": {"type": "string"},
                        },
                        "required": ["code", "message"],
                        "additionalProperties": false,
                    }
                },
                "required": ["error"],
                "additionalProperties": false,
            }
        ]
    })
}

fn project_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "uuid": {"type": "string"},
            "name": {"type": "string"},
        },
        "required": ["uuid", "name"],
        "additionalProperties": false,
    })
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use tokio::io::{
        AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
    };
    use tokio::net::TcpListener;
    use tokio::sync::{mpsc, oneshot};
    use tokio::task::JoinHandle;
    use tokio::time::{Duration, timeout};

    use super::stdio::{MAX_FRAME_BYTES, MAX_IN_FLIGHT_REQUESTS, RESPONSE_QUEUE_CAPACITY, serve};
    use super::*;
    use crate::config::Config;

    const TEST_ACCOUNT: &str = "mcp-test-account";
    const TEST_TOKEN: &str = "mcp-secret-sentinel";
    const EMPTY_PROJECTS_RESPONSE: &str = r#"{"_root":[{"account":"account-id"}],"account":{"account-id":{"projects($limit:100)":[]}},"project":{}}"#;
    const ONE_PROJECT_RESPONSE: &str = r#"{"_root":[{"account":"account-id"}],"account":{"account-id":{"projects($limit:100)":["project-id"]}},"project":{"project-id":{"id":"project-id","name":"Project Name"}}}"#;
    const ALL_ERRORS: [ApplicationError; 11] = [
        ApplicationError::AuthenticationFailed,
        ApplicationError::AuthorizationFailed,
        ApplicationError::ProjectNotFound,
        ApplicationError::ProjectAmbiguous,
        ApplicationError::CardNotFound,
        ApplicationError::CardIdentifierAmbiguous,
        ApplicationError::InvalidIdentifier,
        ApplicationError::Timeout,
        ApplicationError::NetworkFailure,
        ApplicationError::CodecksApiError,
        ApplicationError::InvalidCodecksResponse,
    ];

    struct ProjectServer {
        endpoint: String,
        task: JoinHandle<io::Result<()>>,
    }

    struct StalledProjectServer {
        endpoint: String,
        request_receiver: Option<oneshot::Receiver<()>>,
        task: JoinHandle<io::Result<()>>,
    }

    struct CancellableProjectServer {
        endpoint: String,
        request_receiver: Option<oneshot::Receiver<()>>,
        disconnect_receiver: Option<oneshot::Receiver<()>>,
        task: JoinHandle<io::Result<()>>,
    }

    struct SaturatedProjectServer {
        endpoint: String,
        request_receiver: mpsc::UnboundedReceiver<()>,
        task: JoinHandle<io::Result<()>>,
    }

    struct PendingWriter {
        write_attempt_sender: Option<oneshot::Sender<()>>,
    }

    impl ProjectServer {
        async fn start(body: &'static str) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("the MCP project server should bind to a loopback port");
            let address = listener
                .local_addr()
                .expect("the MCP project server should expose its loopback address");
            let task = tokio::spawn(serve_project_response(listener, body));

            Self {
                endpoint: format!("http://{address}/"),
                task,
            }
        }

        fn endpoint(&self) -> &str {
            &self.endpoint
        }
    }

    impl Drop for ProjectServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    impl StalledProjectServer {
        async fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("the stalled project server should bind to a loopback port");
            let address = listener
                .local_addr()
                .expect("the stalled project server should expose its loopback address");
            let (request_sender, request_receiver) = oneshot::channel();
            let task = tokio::spawn(serve_stalled_project_request(listener, request_sender));

            Self {
                endpoint: format!("http://{address}/"),
                request_receiver: Some(request_receiver),
                task,
            }
        }

        fn endpoint(&self) -> &str {
            &self.endpoint
        }

        async fn wait_for_request(&mut self) {
            timeout(
                Duration::from_secs(2),
                self.request_receiver
                    .take()
                    .expect("the stalled request should only be observed once"),
            )
            .await
            .expect("the MCP tool call should reach the stalled project server")
            .expect("the stalled project server should report the request");
        }
    }

    impl Drop for StalledProjectServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    impl CancellableProjectServer {
        async fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("the cancellable project server should bind to a loopback port");
            let address = listener
                .local_addr()
                .expect("the cancellable project server should expose its loopback address");
            let (request_sender, request_receiver) = oneshot::channel();
            let (disconnect_sender, disconnect_receiver) = oneshot::channel();
            let task = tokio::spawn(serve_cancellable_project_request(
                listener,
                request_sender,
                disconnect_sender,
            ));

            Self {
                endpoint: format!("http://{address}/"),
                request_receiver: Some(request_receiver),
                disconnect_receiver: Some(disconnect_receiver),
                task,
            }
        }

        fn endpoint(&self) -> &str {
            &self.endpoint
        }

        async fn wait_for_request(&mut self) {
            timeout(
                Duration::from_secs(2),
                self.request_receiver
                    .take()
                    .expect("the cancellable request should only be observed once"),
            )
            .await
            .expect("the MCP tool call should reach the cancellable project server")
            .expect("the cancellable project server should report the request");
        }

        async fn wait_for_disconnect(&mut self) {
            timeout(
                Duration::from_secs(2),
                self.disconnect_receiver
                    .take()
                    .expect("the cancellable disconnect should only be observed once"),
            )
            .await
            .expect("the backpressured MCP tool call should be cancelled")
            .expect("the cancellable project server should report the disconnect");
        }
    }

    impl Drop for CancellableProjectServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    impl SaturatedProjectServer {
        async fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("the saturated project server should bind to a loopback port");
            let address = listener
                .local_addr()
                .expect("the saturated project server should expose its loopback address");
            let (request_sender, request_receiver) = mpsc::unbounded_channel();
            let task = tokio::spawn(serve_stalled_project_requests(listener, request_sender));

            Self {
                endpoint: format!("http://{address}/"),
                request_receiver,
                task,
            }
        }

        fn endpoint(&self) -> &str {
            &self.endpoint
        }

        async fn wait_for_requests(&mut self, count: usize) {
            for _ in 0..count {
                timeout(Duration::from_secs(5), self.request_receiver.recv())
                    .await
                    .expect("every permitted request should reach the project server")
                    .expect("the saturated project server should report each request");
            }
        }

        async fn assert_no_more_requests(&mut self) {
            assert!(
                timeout(Duration::from_millis(150), self.request_receiver.recv())
                    .await
                    .is_err(),
                "a tool call exceeded the in-flight request limit"
            );
        }
    }

    impl Drop for SaturatedProjectServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    impl PendingWriter {
        fn new(write_attempt_sender: oneshot::Sender<()>) -> Self {
            Self {
                write_attempt_sender: Some(write_attempt_sender),
            }
        }
    }

    impl AsyncWrite for PendingWriter {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _context: &mut Context<'_>,
            _buffer: &[u8],
        ) -> Poll<io::Result<usize>> {
            if let Some(sender) = self.write_attempt_sender.take() {
                let _ = sender.send(());
            }
            Poll::Pending
        }

        fn poll_flush(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Pending
        }

        fn poll_shutdown(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    fn config() -> Config {
        Config::from_values([
            ("CODECKS_ACCOUNT", TEST_ACCOUNT),
            ("CODECKS_TOKEN", TEST_TOKEN),
        ])
        .expect("the MCP test configuration should be valid")
    }

    fn modern_params() -> Value {
        json!({
            "_meta": {
                "io.modelcontextprotocol/protocolVersion": CURRENT_PROTOCOL_VERSION,
                "io.modelcontextprotocol/clientCapabilities": {},
                "io.modelcontextprotocol/clientInfo": {
                    "name": "mcp-tests",
                    "version": "1.0.0",
                }
            }
        })
    }

    fn request(id: i64, method: &str, params: Value) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        })
    }

    fn client_for(server: &ProjectServer) -> CodecksClient {
        CodecksClient::with_endpoint(&config(), server.endpoint())
            .expect("the MCP test client should build")
    }

    async fn serve_project_response(listener: TcpListener, body: &'static str) -> io::Result<()> {
        let (mut stream, _) = listener.accept().await?;
        read_http_request(&mut stream).await?;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await?;
        stream.shutdown().await
    }

    async fn serve_stalled_project_request(
        listener: TcpListener,
        request_sender: oneshot::Sender<()>,
    ) -> io::Result<()> {
        let (mut stream, _) = listener.accept().await?;
        read_http_request(&mut stream).await?;
        let _ = request_sender.send(());
        tokio::time::sleep(Duration::from_secs(30)).await;
        stream.shutdown().await
    }

    async fn serve_cancellable_project_request(
        listener: TcpListener,
        request_sender: oneshot::Sender<()>,
        disconnect_sender: oneshot::Sender<()>,
    ) -> io::Result<()> {
        let (mut stream, _) = listener.accept().await?;
        read_http_request(&mut stream).await?;
        let _ = request_sender.send(());
        let mut byte = [0_u8; 1];
        let _ = stream.read(&mut byte).await;
        let _ = disconnect_sender.send(());
        Ok(())
    }

    async fn serve_stalled_project_requests(
        listener: TcpListener,
        request_sender: mpsc::UnboundedSender<()>,
    ) -> io::Result<()> {
        loop {
            let (mut stream, _) = listener.accept().await?;
            let request_sender = request_sender.clone();
            drop(tokio::spawn(async move {
                let _ = read_http_request(&mut stream).await;
                let _ = request_sender.send(());
                tokio::time::sleep(Duration::from_secs(30)).await;
                let _ = stream.shutdown().await;
            }));
        }
    }

    async fn read_http_request(stream: &mut tokio::net::TcpStream) -> io::Result<()> {
        let mut request = Vec::new();
        let mut chunk = [0_u8; 4096];

        loop {
            let count = stream.read(&mut chunk).await?;
            if count == 0 {
                return Ok(());
            }
            request.extend_from_slice(&chunk[..count]);

            let Some(headers_end) = request.windows(4).position(|part| part == b"\r\n\r\n") else {
                continue;
            };
            let body_start = headers_end + 4;
            let headers = String::from_utf8_lossy(&request[..headers_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or_default();

            if request.len() >= body_start + content_length {
                return Ok(());
            }
        }
    }

    #[test]
    fn converts_every_application_error_into_a_structured_mcp_error() {
        for application_error in ALL_ERRORS {
            let mcp_error = McpError::from(application_error);

            assert_eq!(mcp_error.code(), application_error.code());
            assert_eq!(mcp_error.message(), application_error.message());
            assert_eq!(mcp_error.to_string(), application_error.message());
        }
    }

    #[test]
    fn mcp_error_output_never_contains_credentials() {
        const CREDENTIAL_SENTINEL: &str = "credential-sentinel";

        for application_error in ALL_ERRORS {
            let mcp_error = McpError::from(application_error);
            let output = format!("{mcp_error:?}\n{mcp_error}");

            assert!(!output.contains(CREDENTIAL_SENTINEL));
            assert_eq!(mcp_error.message(), application_error.message());
        }
    }

    #[tokio::test]
    async fn supports_current_discovery_and_legacy_initialization() {
        let server = ProjectServer::start(EMPTY_PROJECTS_RESPONSE).await;
        let mut mcp = McpServer::new(client_for(&server));

        let discovery = mcp
            .handle_message(request(1, "server/discover", modern_params()))
            .await
            .expect("discovery should return a response");
        let initialization = mcp
            .handle_message(request(
                2,
                "initialize",
                json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "legacy-tests", "version": "1.0.0"},
                }),
            ))
            .await
            .expect("initialization should return a response");

        assert_eq!(
            discovery["result"]["supportedVersions"],
            json!([CURRENT_PROTOCOL_VERSION])
        );
        assert_eq!(discovery["result"]["resultType"], "complete");
        assert_eq!(discovery["result"]["capabilities"], server_capabilities());
        assert_eq!(
            discovery["result"]["_meta"]["io.modelcontextprotocol/serverInfo"],
            server_info()
        );
        assert_eq!(discovery["result"]["ttlMs"], DISCOVERY_TTL_MILLISECONDS);
        assert_eq!(discovery["result"]["cacheScope"], "public");
        assert!(discovery["result"].get("protocolVersion").is_none());
        assert!(discovery["result"].get("serverInfo").is_none());
        assert_eq!(initialization["result"]["protocolVersion"], "2025-06-18");
        assert!(initialization["result"].get("resultType").is_none());
    }

    #[tokio::test]
    async fn returns_the_current_unsupported_protocol_version_error_contract() {
        let server = ProjectServer::start(EMPTY_PROJECTS_RESPONSE).await;
        let mut mcp = McpServer::new(client_for(&server));
        let mut params = modern_params();
        params["_meta"]["io.modelcontextprotocol/protocolVersion"] = json!("1900-01-01");

        let response = mcp
            .handle_message(request(1, "unknown/method", params))
            .await
            .expect("an unsupported protocol version should return an error response");

        assert_eq!(
            response,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "error": {
                    "code": -32022,
                    "message": "Unsupported protocol version.",
                    "data": {
                        "supported": [CURRENT_PROTOCOL_VERSION],
                        "requested": "1900-01-01",
                    }
                }
            })
        );

        let response = mcp
            .handle_message(request(2, "unknown/method", modern_params()))
            .await
            .expect("an unknown method should return an error response");

        assert_eq!(
            response,
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "error": {
                    "code": -32601,
                    "message": "Method not found.",
                }
            })
        );
    }

    #[tokio::test]
    async fn rejects_malformed_idless_messages_but_ignores_valid_notifications() {
        let server = ProjectServer::start(EMPTY_PROJECTS_RESPONSE).await;
        let mut mcp = McpServer::new(client_for(&server));

        for (message, expected_message) in [
            (
                json!({"jsonrpc": "2.0"}),
                "JSON-RPC requests require a method.",
            ),
            (
                json!({"jsonrpc": "1.0", "method": "ping"}),
                "The jsonrpc field must be \"2.0\".",
            ),
            (
                json!({"jsonrpc": "2.0", "method": 42}),
                "JSON-RPC requests require a method.",
            ),
        ] {
            let response = mcp
                .handle_message(message)
                .await
                .expect("a malformed id-less message should return an error response");
            assert_eq!(response["id"], Value::Null);
            assert_eq!(response["error"]["code"], -32600);
            assert_eq!(response["error"]["message"], expected_message);
        }

        assert_eq!(
            mcp.handle_message(json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized",
            }))
            .await,
            None
        );
    }

    #[tokio::test]
    async fn rejects_invalid_request_ids_without_dispatching_tools_or_stalling_eof() {
        let mut project_server = StalledProjectServer::start().await;
        let client = CodecksClient::with_endpoint(&config(), project_server.endpoint())
            .expect("the stalled MCP test client should build");
        let (client_stream, server_stream) = tokio::io::duplex(16 * 1024);
        let (server_reader, server_writer) = tokio::io::split(server_stream);
        let server_task = tokio::spawn(serve(
            McpServer::new(client),
            BufReader::new(server_reader),
            server_writer,
        ));
        let (client_reader, mut client_writer) = tokio::io::split(client_stream);
        let mut client_reader = BufReader::new(client_reader);
        let mut call_params = modern_params();
        call_params
            .as_object_mut()
            .expect("modern params should be an object")
            .extend([
                ("name".to_owned(), json!("list_projects")),
                ("arguments".to_owned(), json!({})),
            ]);

        for invalid_id in [json!(null), json!({"bad": true})] {
            write_test_message(
                &mut client_writer,
                &json!({
                    "jsonrpc": "2.0",
                    "id": invalid_id,
                    "method": "tools/call",
                    "params": call_params,
                }),
            )
            .await;

            let response = read_test_response(&mut client_reader).await;
            assert_eq!(
                response,
                json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": {
                        "code": -32600,
                        "message": "JSON-RPC request IDs must be strings or numbers.",
                    }
                })
            );
        }

        assert!(
            timeout(
                Duration::from_millis(150),
                project_server
                    .request_receiver
                    .take()
                    .expect("the stalled request observation should be available"),
            )
            .await
            .is_err(),
            "an invalid request ID unexpectedly dispatched a Codecks tool call"
        );

        write_test_message(&mut client_writer, &request(3, "ping", modern_params())).await;
        assert_eq!(read_test_response(&mut client_reader).await["id"], 3);
        client_writer
            .shutdown()
            .await
            .expect("the MCP test input should close cleanly");
        wait_for_clean_shutdown(server_task).await;
    }

    #[tokio::test]
    async fn exposes_protocol_conformant_current_and_legacy_tool_definitions() {
        let server = ProjectServer::start(EMPTY_PROJECTS_RESPONSE).await;
        let mut mcp = McpServer::new(client_for(&server));

        let current = mcp
            .handle_message(request(1, "tools/list", modern_params()))
            .await
            .expect("tool discovery should return a response");
        mcp.handle_message(request(
            2,
            "initialize",
            json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "legacy-tool-tests", "version": "1.0.0"},
            }),
        ))
        .await
        .expect("legacy initialization should return a response");
        let legacy = mcp
            .handle_message(request(
                3,
                "tools/list",
                json!({"_meta": {"progressToken": "legacy-tool-tests"}}),
            ))
            .await
            .expect("legacy tool discovery should return a response");

        assert_eq!(current["result"]["ttlMs"], DISCOVERY_TTL_MILLISECONDS);
        assert_eq!(current["result"]["cacheScope"], "public");
        assert_eq!(current["result"]["resultType"], "complete");
        assert!(legacy["result"].get("ttlMs").is_none());
        assert!(legacy["result"].get("cacheScope").is_none());
        assert!(legacy["result"].get("resultType").is_none());

        for response in [&current, &legacy] {
            let tools = response["result"]["tools"]
                .as_array()
                .expect("the tool list should be an array");
            let names = tools
                .iter()
                .map(|tool| tool["name"].as_str().expect("each tool should have a name"))
                .collect::<Vec<_>>();
            assert_eq!(names, ["list_projects", "get_project"]);
            assert!(
                tools
                    .iter()
                    .all(|tool| tool["outputSchema"]["type"] == "object")
            );
        }
    }

    #[tokio::test]
    async fn lists_projects_with_matching_text_and_structured_content() {
        let server = ProjectServer::start(ONE_PROJECT_RESPONSE).await;
        let mut mcp = McpServer::new(client_for(&server));
        let mut params = modern_params();
        params
            .as_object_mut()
            .expect("modern params should be an object")
            .extend([
                ("name".to_owned(), json!("list_projects")),
                ("arguments".to_owned(), json!({})),
            ]);

        let response = mcp
            .handle_message(request(1, "tools/call", params))
            .await
            .expect("the tool call should return a response");
        let result = &response["result"];
        let text: Value = serde_json::from_str(
            result["content"][0]["text"]
                .as_str()
                .expect("the text result should be present"),
        )
        .expect("the text result should contain JSON");

        assert_eq!(result["isError"], false);
        assert_eq!(result["structuredContent"], text);
        assert_eq!(
            result["structuredContent"]["projects"][0],
            json!({"uuid": "project-id", "name": "Project Name"})
        );
    }

    #[tokio::test]
    async fn gets_the_sole_project_without_an_explicit_selector() {
        let server = ProjectServer::start(ONE_PROJECT_RESPONSE).await;
        let mut mcp = McpServer::new(client_for(&server));
        let mut params = modern_params();
        params
            .as_object_mut()
            .expect("modern params should be an object")
            .extend([
                ("name".to_owned(), json!("get_project")),
                ("arguments".to_owned(), json!({})),
            ]);

        let response = mcp
            .handle_message(request(1, "tools/call", params))
            .await
            .expect("the tool call should return a response");

        assert_eq!(
            response["result"]["structuredContent"]["project"],
            json!({"uuid": "project-id", "name": "Project Name"})
        );
        assert_eq!(response["result"]["isError"], false);
    }

    #[tokio::test]
    async fn returns_a_typed_tool_error_when_automatic_resolution_fails() {
        let server = ProjectServer::start(EMPTY_PROJECTS_RESPONSE).await;
        let mut mcp = McpServer::new(client_for(&server));
        let mut params = modern_params();
        params
            .as_object_mut()
            .expect("modern params should be an object")
            .extend([
                ("name".to_owned(), json!("get_project")),
                ("arguments".to_owned(), json!({})),
            ]);

        let response = mcp
            .handle_message(request(1, "tools/call", params))
            .await
            .expect("the tool call should return a response");

        assert_eq!(response["result"]["isError"], true);
        assert_eq!(
            response["result"]["structuredContent"]["error"]["code"],
            "project_not_found"
        );
        assert_eq!(
            response["result"]["structuredContent"]["error"]["message"],
            ApplicationError::ProjectNotFound.message()
        );
    }

    #[tokio::test]
    async fn cancellation_aborts_a_stalled_tool_call_without_emitting_its_response() {
        let mut project_server = StalledProjectServer::start().await;
        let client = CodecksClient::with_endpoint(&config(), project_server.endpoint())
            .expect("the stalled MCP test client should build");
        let (client_stream, server_stream) = tokio::io::duplex(16 * 1024);
        let (server_reader, server_writer) = tokio::io::split(server_stream);
        let server_task = tokio::spawn(serve(
            McpServer::new(client),
            BufReader::new(server_reader),
            server_writer,
        ));
        let (client_reader, mut client_writer) = tokio::io::split(client_stream);
        let mut client_reader = BufReader::new(client_reader);
        let mut call_params = modern_params();
        call_params
            .as_object_mut()
            .expect("modern params should be an object")
            .extend([
                ("name".to_owned(), json!("list_projects")),
                ("arguments".to_owned(), json!({})),
            ]);

        write_test_message(&mut client_writer, &request(1, "tools/call", call_params)).await;
        project_server.wait_for_request().await;
        write_test_message(
            &mut client_writer,
            &json!({
                "jsonrpc": "2.0",
                "method": "notifications/cancelled",
                "params": {"requestId": 1, "reason": "test cancellation"},
            }),
        )
        .await;
        write_test_message(&mut client_writer, &request(2, "ping", modern_params())).await;

        let response = read_test_response(&mut client_reader).await;
        assert_eq!(response["id"], 2);
        assert!(
            timeout(
                Duration::from_millis(150),
                read_test_response(&mut client_reader)
            )
            .await
            .is_err(),
            "the cancelled request unexpectedly emitted a response"
        );

        client_writer
            .shutdown()
            .await
            .expect("the MCP test input should close cleanly");
        wait_for_clean_shutdown(server_task).await;
    }

    #[tokio::test]
    async fn cancellation_is_processed_while_response_output_is_backpressured() {
        let mut project_server = CancellableProjectServer::start().await;
        let client = CodecksClient::with_endpoint(&config(), project_server.endpoint())
            .expect("the cancellable MCP test client should build");
        let (client_stream, server_stream) = tokio::io::duplex(256 * 1024);
        let (server_reader, _) = tokio::io::split(server_stream);
        let (_, mut client_writer) = tokio::io::split(client_stream);
        let (write_attempt_sender, write_attempt_receiver) = oneshot::channel();
        let server_task = tokio::spawn(serve(
            McpServer::new(client),
            BufReader::new(server_reader),
            PendingWriter::new(write_attempt_sender),
        ));
        let mut call_params = modern_params();
        call_params
            .as_object_mut()
            .expect("modern params should be an object")
            .extend([
                ("name".to_owned(), json!("list_projects")),
                ("arguments".to_owned(), json!({})),
            ]);

        write_test_message(&mut client_writer, &request(1, "tools/call", call_params)).await;
        project_server.wait_for_request().await;
        for id in 2..=(RESPONSE_QUEUE_CAPACITY * 2 + 2) as i64 {
            write_test_message(&mut client_writer, &request(id, "ping", modern_params())).await;
        }
        timeout(Duration::from_secs(1), write_attempt_receiver)
            .await
            .expect("the blocked response writer should receive a write attempt")
            .expect("the blocked response writer should report its write attempt");
        write_test_message(
            &mut client_writer,
            &json!({
                "jsonrpc": "2.0",
                "method": "notifications/cancelled",
                "params": {"requestId": 1, "reason": "backpressured cancellation"},
            }),
        )
        .await;

        project_server.wait_for_disconnect().await;
        client_writer
            .shutdown()
            .await
            .expect("the MCP test input should close cleanly");
        wait_for_clean_shutdown(server_task).await;
    }

    #[tokio::test]
    async fn end_of_file_aborts_a_stalled_tool_call_promptly() {
        let mut project_server = StalledProjectServer::start().await;
        let client = CodecksClient::with_endpoint(&config(), project_server.endpoint())
            .expect("the stalled MCP test client should build");
        let (client_stream, server_stream) = tokio::io::duplex(16 * 1024);
        let (server_reader, server_writer) = tokio::io::split(server_stream);
        let server_task = tokio::spawn(serve(
            McpServer::new(client),
            BufReader::new(server_reader),
            server_writer,
        ));
        let (_, mut client_writer) = tokio::io::split(client_stream);
        let mut call_params = modern_params();
        call_params
            .as_object_mut()
            .expect("modern params should be an object")
            .extend([
                ("name".to_owned(), json!("list_projects")),
                ("arguments".to_owned(), json!({})),
            ]);

        write_test_message(&mut client_writer, &request(1, "tools/call", call_params)).await;
        project_server.wait_for_request().await;
        client_writer
            .shutdown()
            .await
            .expect("the MCP test input should close cleanly");

        wait_for_clean_shutdown(server_task).await;
    }

    #[tokio::test]
    async fn end_of_file_aborts_a_permanently_blocked_response_writer() {
        let project_server = ProjectServer::start(EMPTY_PROJECTS_RESPONSE).await;
        let (client_stream, server_stream) = tokio::io::duplex(16 * 1024);
        let (server_reader, _) = tokio::io::split(server_stream);
        let (_, mut client_writer) = tokio::io::split(client_stream);
        let (write_attempt_sender, write_attempt_receiver) = oneshot::channel();
        let server_task = tokio::spawn(serve(
            McpServer::new(client_for(&project_server)),
            BufReader::new(server_reader),
            PendingWriter::new(write_attempt_sender),
        ));

        write_test_message(&mut client_writer, &request(1, "ping", modern_params())).await;
        timeout(Duration::from_secs(1), write_attempt_receiver)
            .await
            .expect("the response writer should attempt to write")
            .expect("the pending writer should report its write attempt");
        client_writer
            .shutdown()
            .await
            .expect("the MCP test input should close cleanly");

        wait_for_clean_shutdown(server_task).await;
    }

    #[tokio::test]
    async fn end_of_file_drains_accepted_responses_when_a_blocked_writer_resumes() {
        let project_server = ProjectServer::start(EMPTY_PROJECTS_RESPONSE).await;
        let (client_input, server_input) = tokio::io::duplex(256 * 1024);
        let (server_reader, _) = tokio::io::split(server_input);
        let (_, mut client_writer) = tokio::io::split(client_input);
        let (server_output, client_output) = tokio::io::duplex(64);
        let mut client_reader = BufReader::new(client_output);
        let server_task = tokio::spawn(serve(
            McpServer::new(client_for(&project_server)),
            BufReader::new(server_reader),
            server_output,
        ));
        let request_count = (RESPONSE_QUEUE_CAPACITY * 2 + 2) as i64;

        for id in 1..=request_count {
            write_test_message(&mut client_writer, &request(id, "ping", modern_params())).await;
        }
        client_writer
            .shutdown()
            .await
            .expect("the MCP test input should close cleanly");
        tokio::time::sleep(Duration::from_millis(20)).await;

        for expected_id in 1..=request_count {
            assert_eq!(
                read_test_response(&mut client_reader).await["id"],
                expected_id,
                "EOF should preserve every response accepted before output resumes"
            );
        }
        wait_for_clean_shutdown(server_task).await;
    }

    #[tokio::test]
    async fn reaps_completed_tool_calls_before_accepting_more_ready_input() {
        let project_server = ProjectServer::start(EMPTY_PROJECTS_RESPONSE).await;
        let (client_stream, server_stream) = tokio::io::duplex(256 * 1024);
        let (server_reader, server_writer) = tokio::io::split(server_stream);
        let server_task = tokio::spawn(serve(
            McpServer::new(client_for(&project_server)),
            BufReader::new(server_reader),
            server_writer,
        ));
        let (client_reader, mut client_writer) = tokio::io::split(client_stream);
        let mut client_reader = BufReader::new(client_reader);
        let mut call_params = modern_params();
        call_params
            .as_object_mut()
            .expect("modern params should be an object")
            .extend([
                ("name".to_owned(), json!("unknown_tool")),
                ("arguments".to_owned(), json!({})),
            ]);

        for id in 1..=MAX_IN_FLIGHT_REQUESTS {
            write_test_message(
                &mut client_writer,
                &request(id as i64, "tools/call", call_params.clone()),
            )
            .await;
        }
        for _ in 0..1_024 {
            write_test_message(
                &mut client_writer,
                &json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/initialized",
                }),
            )
            .await;
        }
        let final_id = (MAX_IN_FLIGHT_REQUESTS + 1) as i64;
        write_test_message(
            &mut client_writer,
            &request(final_id, "tools/call", call_params),
        )
        .await;

        let mut final_response = None;
        for _ in 0..=MAX_IN_FLIGHT_REQUESTS {
            let response = read_test_response(&mut client_reader).await;
            if response["id"] == final_id {
                final_response = Some(response);
            }
        }
        let final_response = final_response.expect("the final tool call should receive a response");
        assert_eq!(final_response["error"]["code"], -32602);
        assert_eq!(final_response["error"]["message"], "Unknown tool name.");

        client_writer
            .shutdown()
            .await
            .expect("the MCP test input should close cleanly");
        wait_for_clean_shutdown(server_task).await;
    }

    #[tokio::test]
    async fn caps_concurrent_tool_calls_and_rejects_excess_work() {
        let mut project_server = SaturatedProjectServer::start().await;
        let client = CodecksClient::with_endpoint(&config(), project_server.endpoint())
            .expect("the saturated MCP test client should build");
        let (client_stream, server_stream) = tokio::io::duplex(64 * 1024);
        let (server_reader, server_writer) = tokio::io::split(server_stream);
        let server_task = tokio::spawn(serve(
            McpServer::new(client),
            BufReader::new(server_reader),
            server_writer,
        ));
        let (client_reader, mut client_writer) = tokio::io::split(client_stream);
        let mut client_reader = BufReader::new(client_reader);
        let mut call_params = modern_params();
        call_params
            .as_object_mut()
            .expect("modern params should be an object")
            .extend([
                ("name".to_owned(), json!("list_projects")),
                ("arguments".to_owned(), json!({})),
            ]);

        for id in 1..=(MAX_IN_FLIGHT_REQUESTS + 1) {
            write_test_message(
                &mut client_writer,
                &request(id as i64, "tools/call", call_params.clone()),
            )
            .await;
        }

        let response = read_test_response(&mut client_reader).await;
        assert_eq!(response["id"], (MAX_IN_FLIGHT_REQUESTS + 1) as i64);
        assert_eq!(response["error"]["code"], -32000);
        assert_eq!(
            response["error"]["message"],
            "Too many tool requests are already in flight."
        );
        project_server
            .wait_for_requests(MAX_IN_FLIGHT_REQUESTS)
            .await;
        project_server.assert_no_more_requests().await;

        client_writer
            .shutdown()
            .await
            .expect("the MCP test input should close cleanly");
        wait_for_clean_shutdown(server_task).await;
    }

    #[tokio::test]
    async fn oversized_frames_fail_safely_and_do_not_stop_later_requests() {
        let project_server = ProjectServer::start(EMPTY_PROJECTS_RESPONSE).await;
        let (client_stream, server_stream) = tokio::io::duplex(64 * 1024);
        let (server_reader, server_writer) = tokio::io::split(server_stream);
        let server_task = tokio::spawn(serve(
            McpServer::new(client_for(&project_server)),
            BufReader::new(server_reader),
            server_writer,
        ));
        let (client_reader, mut client_writer) = tokio::io::split(client_stream);
        let mut client_reader = BufReader::new(client_reader);

        client_writer
            .write_all(&vec![b' '; MAX_FRAME_BYTES + 1])
            .await
            .expect("the oversized test frame should be written");
        client_writer
            .write_all(b"\n")
            .await
            .expect("the oversized test frame should be terminated");
        write_test_message(&mut client_writer, &request(2, "ping", modern_params())).await;

        let oversized_response = read_test_response(&mut client_reader).await;
        let ping_response = read_test_response(&mut client_reader).await;
        assert_eq!(oversized_response["id"], Value::Null);
        assert_eq!(oversized_response["error"]["code"], -32600);
        assert_eq!(
            oversized_response["error"]["message"],
            "The MCP message exceeds the one mebibyte frame limit."
        );
        assert_eq!(ping_response["id"], 2);

        client_writer
            .shutdown()
            .await
            .expect("the MCP test input should close cleanly");
        wait_for_clean_shutdown(server_task).await;
    }

    async fn write_test_message<W>(writer: &mut W, message: &Value)
    where
        W: AsyncWrite + Unpin,
    {
        writer
            .write_all(format!("{message}\n").as_bytes())
            .await
            .expect("the MCP test message should be written");
    }

    async fn read_test_response<R>(reader: &mut R) -> Value
    where
        R: AsyncBufRead + Unpin,
    {
        let mut response = String::new();
        timeout(Duration::from_secs(2), reader.read_line(&mut response))
            .await
            .expect("the MCP response should arrive promptly")
            .expect("the MCP response should be readable");
        serde_json::from_str(&response).expect("the MCP response should be valid JSON")
    }

    async fn wait_for_clean_shutdown(task: JoinHandle<io::Result<()>>) {
        timeout(Duration::from_secs(1), task)
            .await
            .expect("the MCP server should stop promptly after EOF")
            .expect("the MCP server task should join")
            .expect("the MCP server should shut down cleanly");
    }
}
