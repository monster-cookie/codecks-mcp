# codecks-mcp

An independent Model Context Protocol (MCP) server for the Codecks API.

> [!NOTE]
> The repository currently contains the Rust workspace, validated runtime configuration, core
> error model, asynchronous Codecks HTTP client, and paginated project discovery. It does not yet
> implement the MCP protocol or expose project discovery as an MCP operation.

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
- `src/mcp.rs` defines MCP-facing representations and is reserved for transport integration.
- `src/codecks_api.rs` provides authenticated, timeout-bounded Codecks API transport and project
  enumeration.
- `src/config.rs` is reserved for configuration loading and validation.
- `src/domain.rs` defines project models shared across integrations.
- `src/error.rs` defines application error types shared by the API and MCP layers.

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
