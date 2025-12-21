# tests/ notes

- Tests are contract/regression protection (not coverage).
- Prefer black-box CLI assertions via `assert_cmd`.
- Use fixtures for provider/tool-loop parsing edges; avoid real network.
- Set env vars on the spawned `Command` instead of mutating global env in-process.

