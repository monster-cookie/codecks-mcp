//! Codecks API integration.
//!
//! This module isolates Codecks authentication, requests, and responses from MCP protocol handling.

use std::collections::HashSet;
use std::fmt;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Client, StatusCode, Url};
use serde_json::{Map, Value, json};

use crate::config::Config;
use crate::domain::Project;
use crate::error::ApplicationError;

/// The fixed public endpoint used for Codecks API requests.
pub const CODECKS_API_ENDPOINT: &str = "https://api.codecks.io/";

const ACCOUNT_HEADER: HeaderName = HeaderName::from_static("x-account");
const AUTHENTICATION_HEADER: HeaderName = HeaderName::from_static("x-auth-token");
const PROJECT_PAGE_SIZE: usize = 100;

/// An asynchronous, credential-safe client for the Codecks JSON API.
///
/// The default constructor always targets [`CODECKS_API_ENDPOINT`]. Request headers are prepared
/// once and omitted from the client's debug representation so credentials cannot be exposed by
/// routine diagnostics.
#[derive(Clone)]
pub struct CodecksClient {
    http_client: Client,
    endpoint: Url,
    request_headers: HeaderMap,
    request_timeout: Duration,
}

impl CodecksClient {
    /// Builds a client from validated runtime configuration using the fixed Codecks API endpoint.
    ///
    /// # Errors
    ///
    /// Returns [`ApplicationError::InvalidIdentifier`] when the account cannot be represented as
    /// an HTTP header, or [`ApplicationError::AuthenticationFailed`] when the authentication token
    /// cannot be represented safely as an HTTP header.
    pub fn new(config: &Config) -> Result<Self, ApplicationError> {
        let endpoint =
            Url::parse(CODECKS_API_ENDPOINT).map_err(|_| ApplicationError::InvalidIdentifier)?;
        Self::build(config, endpoint)
    }

    #[cfg(test)]
    pub(crate) fn with_endpoint(config: &Config, endpoint: &str) -> Result<Self, ApplicationError> {
        let endpoint = Url::parse(endpoint).map_err(|_| ApplicationError::InvalidIdentifier)?;
        Self::build(config, endpoint)
    }

    fn build(config: &Config, endpoint: Url) -> Result<Self, ApplicationError> {
        let account = HeaderValue::from_str(config.account())
            .map_err(|_| ApplicationError::InvalidIdentifier)?;
        let authentication = HeaderValue::from_str(config.authentication_token().expose())
            .map_err(|_| ApplicationError::AuthenticationFailed)?;
        let mut request_headers = HeaderMap::new();
        request_headers.insert(ACCOUNT_HEADER, account);
        request_headers.insert(AUTHENTICATION_HEADER, authentication);

        let request_timeout = config.request_timeout();
        let http_client = Client::builder()
            .connect_timeout(request_timeout)
            .timeout(request_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| ApplicationError::NetworkFailure)?;

        Ok(Self {
            http_client,
            endpoint,
            request_headers,
            request_timeout,
        })
    }

    /// Sends a JSON request to Codecks and returns its decoded JSON response.
    ///
    /// # Errors
    ///
    /// Returns a typed [`ApplicationError`] for authentication, authorization, timeout, network,
    /// API-status, and invalid-response failures. No response body or upstream error text is
    /// retained in the returned error.
    pub async fn request(&self, request: &Value) -> Result<Value, ApplicationError> {
        let response = self
            .http_client
            .post(self.endpoint.clone())
            .headers(self.request_headers.clone())
            .json(request)
            .send()
            .await
            .map_err(map_request_error)?;

        map_status(response.status())?;

        response.json().await.map_err(map_response_error)
    }

    /// Retrieves every active project available to the authenticated Codecks account.
    ///
    /// Results retain the stable Codecks UUID and the current display name. The API is queried in
    /// deterministic pages until it returns fewer projects than the requested page size.
    ///
    /// # Errors
    ///
    /// Returns the typed transport errors documented by [`Self::request`], or
    /// [`ApplicationError::InvalidCodecksResponse`] when a response does not contain a valid
    /// normalized Codecks project page.
    pub async fn list_projects(&self) -> Result<Vec<Project>, ApplicationError> {
        self.list_projects_with_page_size(PROJECT_PAGE_SIZE).await
    }

    async fn list_projects_with_page_size(
        &self,
        page_size: usize,
    ) -> Result<Vec<Project>, ApplicationError> {
        debug_assert!(page_size > 0, "the project page size must be positive");

        let mut offset = 0;
        let mut projects = Vec::new();
        let mut project_uuids = HashSet::new();

        loop {
            let request = project_page_request(page_size, offset);
            let response = self.request(&request).await?;
            let page = parse_project_page(&response)?;
            let page_length = page.len();

            for project in page {
                if !project_uuids.insert(project.uuid().to_owned()) {
                    return Err(ApplicationError::InvalidCodecksResponse);
                }
                projects.push(project);
            }

            if page_length < page_size {
                return Ok(projects);
            }

            offset = offset
                .checked_add(page_size)
                .ok_or(ApplicationError::InvalidCodecksResponse)?;
        }
    }
}

impl fmt::Debug for CodecksClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodecksClient")
            .field("endpoint", &self.endpoint.as_str())
            .field("request_headers", &"[REDACTED]")
            .field("request_timeout", &self.request_timeout)
            .finish()
    }
}

fn map_status(status: StatusCode) -> Result<(), ApplicationError> {
    match status {
        StatusCode::UNAUTHORIZED => Err(ApplicationError::AuthenticationFailed),
        StatusCode::FORBIDDEN => Err(ApplicationError::AuthorizationFailed),
        StatusCode::REQUEST_TIMEOUT | StatusCode::GATEWAY_TIMEOUT => Err(ApplicationError::Timeout),
        status if status.is_success() => Ok(()),
        _ => Err(ApplicationError::CodecksApiError),
    }
}

fn map_request_error(error: reqwest::Error) -> ApplicationError {
    if error.is_timeout() {
        ApplicationError::Timeout
    } else {
        ApplicationError::NetworkFailure
    }
}

fn map_response_error(error: reqwest::Error) -> ApplicationError {
    if error.is_timeout() {
        ApplicationError::Timeout
    } else if error.is_decode() {
        ApplicationError::InvalidCodecksResponse
    } else {
        ApplicationError::NetworkFailure
    }
}

fn project_page_request(page_size: usize, offset: usize) -> Value {
    let mut project_selection = Map::new();
    project_selection.insert(
        project_relation_key(page_size, offset),
        json!(["id", "name"]),
    );

    json!({
        "query": {
            "_root": [{
                "account": [project_selection]
            }]
        }
    })
}

fn project_relation_key(page_size: usize, offset: usize) -> String {
    let pagination = json!({
        "$order": "id",
        "$limit": page_size,
        "$offset": offset,
    });
    format!("projects({pagination})")
}

fn parse_project_page(response: &Value) -> Result<Vec<Project>, ApplicationError> {
    let root = response
        .get("_root")
        .and_then(|root| {
            root.as_array()
                .and_then(|entries| entries.first())
                .or(Some(root))
        })
        .and_then(Value::as_object)
        .ok_or(ApplicationError::InvalidCodecksResponse)?;
    let account_reference =
        relation_value(root, "account").ok_or(ApplicationError::InvalidCodecksResponse)?;
    let account = resolve_entity(response, "account", account_reference)?;
    let project_references = relation_value(account, "projects")
        .and_then(Value::as_array)
        .ok_or(ApplicationError::InvalidCodecksResponse)?;

    project_references
        .iter()
        .map(|project_reference| {
            let project_entity = resolve_entity(response, "project", project_reference)?;
            let project = serde_json::from_value::<Project>(Value::Object(project_entity.clone()))
                .map_err(|_| ApplicationError::InvalidCodecksResponse)?;

            if let Some(project_uuid) = project_reference.as_str()
                && project.uuid() != project_uuid
            {
                return Err(ApplicationError::InvalidCodecksResponse);
            }

            Ok(project)
        })
        .collect()
}

fn relation_value<'a>(entity: &'a Map<String, Value>, relation: &str) -> Option<&'a Value> {
    entity.get(relation).or_else(|| {
        let queried_relation_prefix = format!("{relation}(");
        entity
            .iter()
            .find_map(|(key, value)| key.starts_with(&queried_relation_prefix).then_some(value))
    })
}

fn resolve_entity<'a>(
    response: &'a Value,
    model: &str,
    reference: &'a Value,
) -> Result<&'a Map<String, Value>, ApplicationError> {
    if let Some(entity) = reference.as_object() {
        return Ok(entity);
    }

    let entity_id = reference
        .as_str()
        .ok_or(ApplicationError::InvalidCodecksResponse)?;
    response
        .get(model)
        .and_then(Value::as_object)
        .and_then(|entities| entities.get(entity_id))
        .and_then(Value::as_object)
        .ok_or(ApplicationError::InvalidCodecksResponse)
}

#[cfg(test)]
#[path = "../tests/support/mod.rs"]
mod support;

#[cfg(test)]
mod tests {
    use std::io;
    use std::time::Duration;

    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;
    use tokio::task::JoinHandle;
    use tokio::time::timeout;

    use super::support::{DisconnectingServer, MockResponse, MockServer};
    use super::*;

    const TEST_ACCOUNT: &str = "client-test-account";
    const TEST_TOKEN: &str = "client-secret-sentinel";

    struct ProjectPageServer {
        endpoint: String,
        request_receiver: Option<oneshot::Receiver<Vec<Vec<u8>>>>,
        task: JoinHandle<io::Result<()>>,
    }

    impl ProjectPageServer {
        async fn start(responses: Vec<String>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("the project mock server should bind to a loopback port");
            let address = listener
                .local_addr()
                .expect("the project mock server should expose its loopback address");
            let (request_sender, request_receiver) = oneshot::channel();
            let task = tokio::spawn(serve_project_pages(listener, responses, request_sender));

            Self {
                endpoint: format!("http://{address}/"),
                request_receiver: Some(request_receiver),
                task,
            }
        }

        fn endpoint(&self) -> &str {
            &self.endpoint
        }

        async fn received_requests(&mut self) -> Vec<Vec<u8>> {
            self.request_receiver
                .take()
                .expect("the project requests should only be read once")
                .await
                .expect("the project mock server should capture its requests")
        }
    }

    impl Drop for ProjectPageServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    fn config(timeout_seconds: &str) -> Config {
        Config::from_values([
            ("CODECKS_ACCOUNT", TEST_ACCOUNT),
            ("CODECKS_TOKEN", TEST_TOKEN),
            ("CODECKS_TIMEOUT_SECONDS", timeout_seconds),
        ])
        .expect("the client test configuration should be valid")
    }

    fn project_page_response(page_size: usize, offset: usize, projects: &[(&str, &str)]) -> String {
        let project_ids = projects
            .iter()
            .map(|(project_id, _)| Value::String((*project_id).to_owned()))
            .collect();
        let mut account = Map::new();
        account.insert(
            project_relation_key(page_size, offset),
            Value::Array(project_ids),
        );
        let project_entities = projects
            .iter()
            .map(|(project_id, name)| {
                (
                    (*project_id).to_owned(),
                    json!({"id": project_id, "name": name}),
                )
            })
            .collect::<Map<_, _>>();

        json!({
            "_root": [{"account": "account-id"}],
            "account": {"account-id": account},
            "project": project_entities,
        })
        .to_string()
    }

    fn requested_project_relation(request: &[u8]) -> String {
        let body_start = request
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("the captured request should contain HTTP headers")
            + 4;
        let body: Value = serde_json::from_slice(&request[body_start..])
            .expect("the captured request body should be JSON");

        body["query"]["_root"][0]["account"][0]
            .as_object()
            .and_then(|selection| selection.keys().next())
            .cloned()
            .expect("the request should select the projects relation")
    }

    #[tokio::test]
    async fn sends_authenticated_json_and_decodes_successful_response() {
        let mut server =
            MockServer::start(MockResponse::json(200, r#"{"result":{"ok":true}}"#)).await;
        let client = CodecksClient::with_endpoint(&config("30"), server.endpoint())
            .expect("the test client should build");

        let response = client
            .request(&json!({"queries": [{"_root": ["account", "projects"]}]}))
            .await
            .expect("a valid response should decode");
        let request = String::from_utf8(server.received_request().await)
            .expect("the captured request should be UTF-8");
        let normalized_request = request.to_ascii_lowercase();

        assert_eq!(response, json!({"result": {"ok": true}}));
        assert!(normalized_request.starts_with("post / http/1.1\r\n"));
        assert!(normalized_request.contains("x-account: client-test-account\r\n"));
        assert!(normalized_request.contains("x-auth-token: client-secret-sentinel\r\n"));
        assert!(request.contains(r#""queries"#));
    }

    #[tokio::test]
    async fn maps_authentication_and_authorization_statuses() {
        for (status, expected_error) in [
            (401, ApplicationError::AuthenticationFailed),
            (403, ApplicationError::AuthorizationFailed),
        ] {
            let server =
                MockServer::start(MockResponse::json(status, r#"{"error":"ignored"}"#)).await;
            let client = CodecksClient::with_endpoint(&config("30"), server.endpoint())
                .expect("the test client should build");

            let error = client
                .request(&json!({"queries": []}))
                .await
                .expect_err("the HTTP status should map to a typed error");

            assert_eq!(error, expected_error);
        }
    }

    #[tokio::test]
    async fn maps_network_and_timeout_failures() {
        let disconnecting_server = DisconnectingServer::start().await;
        let network_client =
            CodecksClient::with_endpoint(&config("30"), disconnecting_server.endpoint())
                .expect("the network-failure client should build");
        let network_error = network_client
            .request(&json!({"queries": []}))
            .await
            .expect_err("a disconnected request should fail");

        assert_eq!(network_error, ApplicationError::NetworkFailure);

        let server = MockServer::start(
            MockResponse::json(200, r#"{"result":{}}"#).delayed(Duration::from_secs(2)),
        )
        .await;
        let timeout_client = CodecksClient::with_endpoint(&config("1"), server.endpoint())
            .expect("the timeout client should build");
        let timeout_error = timeout_client
            .request(&json!({"queries": []}))
            .await
            .expect_err("a delayed response should time out");

        assert_eq!(timeout_error, ApplicationError::Timeout);
    }

    #[tokio::test]
    async fn rejects_invalid_json_without_retaining_the_response_body() {
        const INVALID_RESPONSE_SENTINEL: &str = "invalid-response-secret-sentinel";

        let server = MockServer::start(MockResponse::json(200, INVALID_RESPONSE_SENTINEL)).await;
        let client = CodecksClient::with_endpoint(&config("30"), server.endpoint())
            .expect("the test client should build");
        let error = client
            .request(&json!({"queries": []}))
            .await
            .expect_err("invalid JSON should fail safely");
        let diagnostics = format!("{error:?}\n{error}");

        assert_eq!(error, ApplicationError::InvalidCodecksResponse);
        assert!(!diagnostics.contains(INVALID_RESPONSE_SENTINEL));
    }

    #[tokio::test]
    async fn rejects_redirect_without_contacting_or_authenticating_the_target() {
        let mut redirect_target =
            MockServer::start(MockResponse::json(200, r#"{"result":{}}"#)).await;
        let mut redirecting_server =
            MockServer::start(MockResponse::redirect(redirect_target.endpoint())).await;
        let client = CodecksClient::with_endpoint(&config("30"), redirecting_server.endpoint())
            .expect("the redirect test client should build");

        let error = client
            .request(&json!({"queries": []}))
            .await
            .expect_err("a redirect should remain an API status failure");
        let initial_request = String::from_utf8(redirecting_server.received_request().await)
            .expect("the initial request should be UTF-8");

        assert_eq!(error, ApplicationError::CodecksApiError);
        assert!(initial_request.contains(TEST_TOKEN));
        assert!(
            timeout(
                Duration::from_millis(150),
                redirect_target.received_request()
            )
            .await
            .is_err(),
            "the redirect target unexpectedly received a request"
        );
    }

    #[tokio::test]
    async fn lists_zero_projects_from_a_mocked_api_page() {
        let mut server = ProjectPageServer::start(vec![project_page_response(2, 0, &[])]).await;
        let client = CodecksClient::with_endpoint(&config("30"), server.endpoint())
            .expect("the project test client should build");

        let projects = client
            .list_projects_with_page_size(2)
            .await
            .expect("an empty project page should succeed");
        let requests = server.received_requests().await;

        assert!(projects.is_empty());
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requested_project_relation(&requests[0]),
            project_relation_key(2, 0)
        );
    }

    #[tokio::test]
    async fn lists_one_project_from_a_mocked_api_page() {
        let server = ProjectPageServer::start(vec![project_page_response(
            2,
            0,
            &[("project-uuid", "Project Display Name")],
        )])
        .await;
        let client = CodecksClient::with_endpoint(&config("30"), server.endpoint())
            .expect("the project test client should build");

        let projects = client
            .list_projects_with_page_size(2)
            .await
            .expect("a single project page should succeed");

        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].uuid(), "project-uuid");
        assert_eq!(projects[0].name(), "Project Display Name");
    }

    #[tokio::test]
    async fn retrieves_multiple_project_pages_from_a_mocked_api() {
        let mut server = ProjectPageServer::start(vec![
            project_page_response(
                2,
                0,
                &[("project-a", "Project A"), ("project-b", "Project B")],
            ),
            project_page_response(2, 2, &[("project-c", "Project C")]),
        ])
        .await;
        let client = CodecksClient::with_endpoint(&config("30"), server.endpoint())
            .expect("the project test client should build");

        let projects = client
            .list_projects_with_page_size(2)
            .await
            .expect("all project pages should succeed");
        let requests = server.received_requests().await;

        assert_eq!(
            projects
                .iter()
                .map(|project| (project.uuid(), project.name()))
                .collect::<Vec<_>>(),
            vec![
                ("project-a", "Project A"),
                ("project-b", "Project B"),
                ("project-c", "Project C"),
            ]
        );
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requested_project_relation(&requests[0]),
            project_relation_key(2, 0)
        );
        assert_eq!(
            requested_project_relation(&requests[1]),
            project_relation_key(2, 2)
        );
    }

    async fn serve_project_pages(
        listener: TcpListener,
        responses: Vec<String>,
        request_sender: oneshot::Sender<Vec<Vec<u8>>>,
    ) -> io::Result<()> {
        let mut requests = Vec::with_capacity(responses.len());

        for response in responses {
            let (mut stream, _) = listener.accept().await?;
            requests.push(read_project_request(&mut stream).await?);
            let message = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                response.len()
            );
            stream.write_all(message.as_bytes()).await?;
            stream.shutdown().await?;
        }

        let _ = request_sender.send(requests);
        Ok(())
    }

    async fn read_project_request(stream: &mut tokio::net::TcpStream) -> io::Result<Vec<u8>> {
        let mut request = Vec::new();
        let mut chunk = [0_u8; 4096];

        loop {
            let count = stream.read(&mut chunk).await?;
            if count == 0 {
                break;
            }
            request.extend_from_slice(&chunk[..count]);

            let Some(headers_end) = request.windows(4).position(|window| window == b"\r\n\r\n")
            else {
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
                return Ok(request);
            }
        }

        Ok(request)
    }
}
