# zdx-tools crate

Leaf tool implementations that only need a root directory and optional timeout — no engine, config, or thread state.

## Layout

- `src/lib.rs`: minimal `ToolContext`, serde helpers (`string_or_vec`, `bool_or_string`, `i64_or_string`, `u64_or_string`), path resolution helpers, image path helpers
- `src/bash.rs`: shell command execution, including the auto-background handoff (a foreground command that outruns its bound is relocated, never killed). The engine decides per surface whether a `Handoff` is supplied at all; when it is `None` the command keeps waiting in the foreground.
- `src/adopted.rs`: Unix process-global registry of handed-off jobs; holds each job's lease for the life of the process, so adopted jobs are session-scoped and are terminated by closing that lease. A process that adopts must either outlive its runs or call `drain()` before exiting — dropping the lease (including by dropping the tokio runtime) kills the job. `drain()` also awaits each job's reader tasks, so logs are complete when it returns.
- `src/process_supervisor.rs`: Unix lifetime supervision for invocation-owned process groups (owner lease + external supervisor with TERM/KILL/reaping, target identity reported at startup, bound-aware waits)
- `src/edit.rs`: exact string replacement in files
- `src/file_lock.rs`: per-canonical-path in-process mutex; `edit`, `write`, and `apply_patch` hold it across their read→write so concurrent tool calls on the same file serialize (different files stay parallel)
- `src/write.rs`: file writing
- `src/read.rs`: file reading (text + images)
- `src/image_downscale.rs`: pixel-dimension clamp for provider-bound images (`downscale_for_provider`, `MAX_PROVIDER_IMAGE_EDGE`); re-exported as `zdx_engine::images::downscale_for_provider`
- `src/glob.rs`: file discovery by name pattern
- `src/grep.rs`: regex search across files
- `src/walk.rs`: shared traversal policy for `glob`/`grep` — the wall-clock `WalkBudget` (5s, sliceable into phases), `.git` pruning, and the shallow-then-parallel `walk()`
- `src/web_search.rs`: web search via Parallel API
- `src/fetch_webpage.rs`: URL content extraction via Parallel API
- `src/apply_patch/`: unified diff patch application

## Key types

- `ToolContext` — minimal context: `root: PathBuf` + optional timeout/cancellation
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
- Anywhere image bytes become base64 bound for a provider, clamp pixel dimensions with `image_downscale::downscale_for_provider` — byte-size caps do not cover it, because a long screenshot compresses small while staying tens of thousands of pixels tall. Vision APIs cap dimensions separately (Anthropic: 8000px, and a stricter cap on every image once a request carries more than 20 image blocks), and an oversized `tool_result` image is rejected outright rather than downscaled server-side, failing the whole turn. Current ingress points: `read.rs`, `zdx-bot` Telegram photo/document ingest, `zdx-tui` attachment ingest.
- Image bytes never round-trip through disk (`thread_persistence/replay.rs` drops `ChatContentBlock::Image`), so history replay cannot reintroduce an oversized image — but that says nothing about a live process. `zdx-bot` rebuilds messages from the thread log every turn (`handlers/message/turn.rs` → `load_thread_state`), so its memory is clean by construction; `zdx-tui` instead keeps `thread.messages` across turns (`features/thread/state.rs`) and clones it per run (`runtime/handlers/agent.rs`), so a block attached in that session is re-sent on every later turn until the session restarts or the thread is reloaded. Reason about these two separately.
- Re-encoding a downscaled image must respect the byte budget the caller already checked the original against, and that bound is hard, not best effort: `downscale_for_provider` prefers JPEG, steps down a quality ladder, then halves the edge limit until the encoding fits. Each step was measured, not assumed: a 2000x1333 noise frame is ~6.4MB as PNG vs ~1.6MB as JPEG, and pure 2000x2000 noise is still ~5.7MB at quality 90. Neither a fixed format nor a fixed quality bounds incompressible content — only dropping pixels does.
