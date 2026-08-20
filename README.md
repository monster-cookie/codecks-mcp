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
