# codecks-mcp

An independent Model Context Protocol (MCP) server for the Codecks API.

> [!NOTE]
> The repository currently contains the initial Rust workspace and module boundaries. It does
> not yet implement the MCP protocol or communicate with Codecks.

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
- `src/mcp.rs` is reserved for MCP transport and protocol integration.
- `src/codecks_api.rs` is reserved for Codecks API integration.
- `src/config.rs` is reserved for configuration loading and validation.
- `src/domain.rs` is reserved for domain models shared across integrations.
- `src/error.rs` is reserved for application error types and reporting.

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
