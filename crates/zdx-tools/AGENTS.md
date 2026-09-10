# zdx-tools crate

Leaf tool implementations that only need a root directory and optional timeout — no engine, config, or thread state.

## Layout

- `src/lib.rs`: minimal `ToolContext`, serde helpers (`string_or_vec`, `bool_or_string`, `i64_or_string`, `u64_or_string`), path resolution helpers, image path helpers
- `src/bash.rs`: shell command execution
- `src/edit.rs`: exact string replacement in files
- `src/file_lock.rs`: per-canonical-path in-process mutex; `edit`, `write`, and `apply_patch` hold it across their read→write so concurrent tool calls on the same file serialize (different files stay parallel)
- `src/write.rs`: file writing
- `src/read.rs`: file reading (text + images)
- `src/glob.rs`: file discovery by name pattern
- `src/grep.rs`: regex search across files
- `src/walk.rs`: shared traversal policy for `glob`/`grep` — the wall-clock `WalkBudget` (5s, sliceable into phases), `.git` pruning, and the shallow-then-parallel `walk()`
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
- Any filesystem traversal goes through `walk::walk` with a `WalkPolicy`: it is the only place that decides hidden traversal, gitignore, `.git` pruning, and the time budget, so `glob` and `grep` cannot drift. Hidden files are always searched (dotted paths are ordinary content, and `rg`/`fd` are run with `--hidden` by comparable agents); `.git` is the only hardcoded prune, and it lifts when the caller's pattern names it. Everything else — `node_modules`, `target`, caches — is gitignore's job, never a denylist.
- A traversal that hits the budget must return what it has with `truncated: true` and a `warning` naming the cutoff, never an error and never a silent empty result. The warning must not claim absence, and must point at a deeper `path` rather than a narrower pattern: walk cost tracks tree size, not pattern width.
- Tool descriptions carry the routing contract. A subagent renders its own body plus the tool schemas and never `system_prompt_template.md`, so a tool's description is the only place its capability boundary is stated for that caller. Describe what the tool covers and where a shell is genuinely required; keep it abstract rather than a blacklist of shell commands, which oversteers models.
- `glob` returns files and directories by default and accepts `max_depth`. Discovery that cannot answer "what is one level below this directory" pushes callers to the shell for structure: in one observed explorer run, an opening `glob` that returned 500 deep files and no directories was followed by 15 of 21 `bash` calls doing filesystem traversal.
