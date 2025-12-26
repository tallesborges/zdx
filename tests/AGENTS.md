# tests/ notes

- Tests are contract/regression protection (not coverage).
- Prefer black-box CLI assertions via `assert_cmd`.
- Use fixtures for provider/tool-loop parsing edges; avoid real network.
- Set env vars on the spawned `Command` instead of mutating global env in-process.

## Test isolation guards

The codebase has guards to prevent tests from accidentally hitting real resources:

### Network requests (`AnthropicClient`)

1. **Unit tests (`#[cfg(test)]`)**: `AnthropicClient::new()` panics if `base_url` equals `https://api.anthropic.com`.

2. **Integration tests**: Set `ZDX_BLOCK_REAL_API=1` env var for extra safety. The client will panic if this is set and `base_url` is the production API.

### Session file creation (`Session`)

1. **Unit tests (`#[cfg(test)]`)**: `Session::new()` and `Session::with_id()` panic if `ZDX_HOME` is not set (would use user's home directory).

2. **Integration tests**: Set `ZDX_BLOCK_SESSION_WRITES=1` env var for extra safety.

### Required for all integration tests

- Always set `ANTHROPIC_BASE_URL` to a mock server (wiremock)
- Always set `ZDX_HOME` to a temp directory for full isolation
- Use `--no-save` flag OR set `ZDX_HOME` to prevent session file creation
