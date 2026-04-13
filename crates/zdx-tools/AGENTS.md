# zdx-tools crate

Leaf tool implementations that only need a root directory and optional timeout — no engine, config, or thread state.

## Layout

- `src/lib.rs`: minimal `ToolContext`, serde helpers (`string_or_vec`, `bool_or_string`), path resolution helpers, image path helpers
- `src/bash.rs`: shell command execution
- `src/edit.rs`: exact string replacement in files
- `src/write.rs`: file writing
- `src/read.rs`: file reading (text + images)
- `src/glob.rs`: file discovery by name pattern
- `src/grep.rs`: regex search across files
- `src/web_search.rs`: web search via Parallel API
- `src/fetch_webpage.rs`: URL content extraction via Parallel API
- `src/apply_patch/`: unified diff patch application

## Key types

- `ToolContext` — minimal context: `root: PathBuf` + `timeout: Option<Duration>`
- Re-exports from `zdx-types`: `ToolDefinition`, `ToolResult`, `ToolOutput`, `ImageContent`, etc.

## Conventions

- All leaf tool `execute` functions take `(&Value, &ToolContext)` → `ToolOutput`
- `bash::run` is the async variant; `bash::execute` is the sync wrapper
- Path helpers (`expand_env_vars`, `resolve_existing_path`, etc.) are public for reuse
- Engine-backed tools (read_thread, subagent, thread_search, todo_write) stay in `zdx-engine`
