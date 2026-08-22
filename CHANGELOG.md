# Changelog

## Unreleased

- Add an asynchronous stdio MCP server with schema-valid current discovery, legacy initialization,
  deterministic `list_projects` and `get_project` tools, structured results, bounded input frames,
  protocol-conformant current and legacy tool discovery, bounded asynchronous response
  backpressure without fixed response deadlines, bounded input backlog and tool concurrency, strict
  request-ID validation, in-flight cancellation, and prompt EOF shutdown even when standard output
  is blocked.
- Add deterministic project resolution by explicit UUID, exact display name, or sole-project
  fallback with stable not-found and ambiguity errors.
- Add stable project models and complete paginated Codecks project enumeration with mocked API
  coverage.
- Add an asynchronous, authenticated Codecks HTTP client with bounded timeouts, typed failures, and
  mock transport coverage.
- Add credential-safe application errors and structured MCP-facing error conversion.
- Add validated runtime configuration loading with secret redaction and startup diagnostics.
