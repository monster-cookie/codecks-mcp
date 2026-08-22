# codecks-mcp

An independent Model Context Protocol (MCP) server for the Codecks API.

The repository provides a native Rust MCP server that connects Codex and other MCP clients to the
Codecks API over standard input and standard output.

## Development

Windows is a supported development platform. The commands below run in PowerShell as well as in
other shells supported by Cargo.

### Prerequisites

- [Rustup](https://rustup.rs/) with the current stable Rust toolchain.
- The platform build tools required by Rust. On Windows, use the MSVC Rust toolchain and Visual
  Studio Build Tools.

The checked-in `rust-toolchain.toml` selects stable Rust and installs the Rustfmt and Clippy
components automatically through Rustup.

### Workspace layout

- `src/main.rs` is the executable entry point.
- `src/lib.rs` exposes the library's architectural boundaries.
- `src/mcp.rs` implements MCP discovery, legacy initialization, and tool handling.
- `src/mcp/stdio.rs` implements bounded stdio transport and process lifecycle behavior.
- `src/codecks_api.rs` provides authenticated, timeout-bounded Codecks API transport and project
  enumeration.
- `src/config.rs` is reserved for configuration loading and validation.
- `src/domain.rs` defines project models shared across integrations.
- `src/error.rs` defines application error types shared by the API and MCP layers.
- `src/project_resolver.rs` resolves explicit or implicit project selections for shared callers.

### Error model

Core failures use typed application errors with stable machine-readable codes and credential-safe
messages. Every application error converts into a structured MCP-facing error. These types do not
retain raw credentials, request headers, response bodies, or upstream error text, so standard debug
and display formatting cannot expose those values.

### Build and verify

Run the complete local verification suite from the repository root:

```powershell
cargo build --locked --workspace
cargo test --locked --workspace
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
```

`Cargo.lock` is committed so local and automated builds resolve the same dependency versions.

## Configuration

The executable validates its Codecks configuration before server startup. Configuration errors are
written to standard error and return a failing exit code; standard output remains reserved for MCP
protocol traffic.

| Environment variable | Required | Default | Description |
| --- | --- | --- | --- |
| `CODECKS_ACCOUNT` | Yes | - | Codecks account name or subdomain. |
| `CODECKS_TOKEN` | Yes | - | Authentication token used for Codecks API requests. |
| `CODECKS_TIMEOUT_SECONDS` | No | `30` | Positive integer request timeout in seconds. |
| `CODECKS_LOG_LEVEL` | No | `info` | Diagnostic level: `error`, `warn`, `info`, `debug`, or `trace`. |

Authentication tokens are redacted from standard debug and display output. Code that accesses the
token for authentication must not log or include the exposed value in diagnostics. Tests can load
configuration from supplied name-value pairs without setting real process credentials.

## Codecks HTTP transport

Production requests are sent as JSON `POST` requests to `https://api.codecks.io/` with the account
and authentication token supplied through the Codecks authentication headers. Both connection and
request duration are bounded by `CODECKS_TIMEOUT_SECONDS`. HTTP status, timeout, network, and JSON
decoding failures map to credential-safe application errors; response bodies and upstream error
details are not retained in diagnostics. Redirects are rejected so authentication headers cannot
leave the fixed Codecks endpoint.

## Project discovery

`CodecksClient::list_projects` retrieves every active project visible to the authenticated account.
It requests projects in stable, ordered pages and continues until Codecks returns a partial page.
Each result preserves the stable project UUID and the exact current display name returned by the
API. Mock transport coverage verifies empty, single-project, and multi-page responses without using
live Codecks credentials.

`resolve_project` selects explicit project UUIDs before exact display names. When no selector is
provided, it automatically selects the project only when exactly one is available; empty and
ambiguous selections return the stable `project_not_found` and `project_ambiguous` errors.

## MCP server

Run `codecks-mcp` with the required configuration in its process environment and configure the MCP
client to communicate with the executable over stdio. The server implements current
`2026-07-28` `server/discover` negotiation and legacy `initialize` negotiation for compatible MCP
clients. It emits only newline-delimited JSON-RPC messages on standard output, reports startup and
transport diagnostics on standard error, and limits each input frame to one mebibyte. At most 32
tool calls run concurrently, and queued responses use bounded buffers. Transient output pressure
applies asynchronous backpressure without a fixed response deadline, so a client that continues
reading receives every response even when its buffered processing is slow. Input backlog is bounded
by both message count and retained bytes; a client that continues sending while output is genuinely
blocked receives an explicit transport failure instead of causing memory growth. Tool calls can
complete out of order;
`notifications/cancelled` stops matching in-flight work without sending a response. Closing
standard input cancels outstanding requests and time-bounds response draining so a blocked output
stream cannot prevent process exit.

JSON-RPC request IDs must be strings or numbers. Requests containing `null`, object, array, or
Boolean IDs are rejected as invalid requests before method dispatch and never reach Codecks.

Tool discovery exposes exactly two read-only operations in stable order:

- `list_projects` lists every active project available to the authenticated Codecks account.
- `get_project` accepts an optional `project` UUID or exact display name. Without a selector it
  succeeds only when the account has exactly one project.

Current-protocol tool discovery marks this static, account-independent catalog as publicly
cacheable for one hour. Legacy tool discovery omits current-only cache metadata while retaining
root-object output schemas required by legacy clients.

Both tools return matching JSON text and structured content. Codecks and resolution failures use
the stable credential-safe application error codes and set the MCP tool result's `isError` flag.
