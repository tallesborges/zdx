# tests/ notes

- Tests are contract/regression protection (not coverage).
- Prefer black-box CLI assertions via `assert_cmd`.
- Use fixtures for provider/tool-loop parsing edges; avoid real network.
- Set env vars on the spawned `Command` instead of mutating global env in-process.

## Network request guards

The codebase has two guards to prevent tests from accidentally hitting the real Anthropic API:

1. **Unit tests (`#[cfg(test)]`)**: `AnthropicClient::new()` panics if `base_url` equals `https://api.anthropic.com`.

2. **Integration tests**: Set `ZDX_BLOCK_REAL_API=1` env var on spawned commands for extra safety. The client will panic if this is set and `base_url` is the production API.

**Required for all integration tests:**
- Always set `ANTHROPIC_BASE_URL` to a mock server (wiremock)
- Use `--no-save` flag to prevent session file creation
- Set `ZDX_HOME` to a temp directory for full isolation
