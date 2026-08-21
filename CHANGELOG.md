# Changelog

## Unreleased

- Add deterministic project resolution by explicit UUID, exact display name, or sole-project
  fallback with stable not-found and ambiguity errors.
- Add stable project models and complete paginated Codecks project enumeration with mocked API
  coverage.
- Add an asynchronous, authenticated Codecks HTTP client with bounded timeouts, typed failures, and
  mock transport coverage.
- Add credential-safe application errors and structured MCP-facing error conversion.
- Add validated runtime configuration loading with secret redaction and startup diagnostics.
