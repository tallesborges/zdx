# zdx-tools crate

Leaf tool implementations that only need a root directory and optional timeout — no engine, config, or thread state.

## Layout

- `src/lib.rs`: minimal `ToolContext`, serde helpers (`string_or_vec`, `bool_or_string`, `i64_or_string`, `u64_or_string`), path resolution helpers, image path helpers
- `src/bash.rs`: shell command execution, in two modes chosen by the engine's per-run policy. `execute`: the auto-background handoff (a foreground command that outruns its bound is relocated, never killed); the engine decides per surface whether a `Handoff` is supplied at all, and when it is `None` the command keeps waiting in the foreground. `execute_without_background`: no background execution, so the command stays in the foreground until it finishes; it has no implicit limit and is killed with its process group only at the caller's `timeout_secs` or the run deadline, whichever is earlier. Each mode has its own tool definition (`definition` / `definition_without_background`), so the model is never told about tools its run lacks. Stream readers take raw byte lines (`read_until`): a reader that stops on invalid UTF-8 closes the pipe and kills the command with SIGPIPE (seen with binary output). Results decode lossily; spill files and handoff logs keep the raw bytes. A cancelled command writes no spill files.
- `src/adopted.rs`: Unix process-global registry of handed-off jobs; holds each job's lease for the life of the process, so adopted jobs are session-scoped and are terminated by closing that lease. A process that adopts must either outlive its runs or call `drain()` before exiting — dropping the lease (including by dropping the tokio runtime) kills the job. `drain()` also awaits each job's reader tasks, so logs are complete when it returns. The engine also adopts background subagents here and stops them with `request_terminate` (non-blocking; the drain then waits for the teardown). `claim_terminate` stops a job only if it is still running and no stop has started, so a caller can tell whether it caused the stop.
- `src/process_supervisor.rs`: Unix lifetime supervision for invocation-owned process groups (owner lease + external supervisor with TERM/KILL/reaping, target identity reported at startup, bound-aware waits, `keep_until_exit` values released only after the target is reaped, even when the handle is dropped first)
- `src/edit.rs`: exact string replacement in files
- `src/file_lock.rs`: per-canonical-path in-process mutex; `edit`, `write`, and `apply_patch` hold it across their read→write so concurrent tool calls on the same file serialize (different files stay parallel)
- `src/write.rs`: file writing
- `src/read.rs`: file reading (text + images)
- `src/image_downscale.rs`: pixel-dimension clamp for provider-bound images (`downscale_for_provider`, `MAX_PROVIDER_IMAGE_EDGE`) and `downscale_to_fit` for callers that want tighter edge/byte limits (applies EXIF orientation when it re-encodes); re-exported from `zdx_engine::images`
- `src/glob.rs`: file discovery by name pattern
- `src/grep.rs`: regex search across files
- `src/walk.rs`: shared traversal policy for `glob`/`grep` — the wall-clock `WalkBudget` (5s, sliceable into phases), `.git` pruning, and the shallow-then-parallel `walk()`
- `src/web_search.rs`: web search via Parallel API
- `src/fetch_webpage.rs`: URL content extraction via Parallel API
- `src/parallel.rs`: shared Parallel.ai HTTP client, retry policy, and error reporting for `web_search` and `fetch_webpage`
- `src/apply_patch/`: unified diff patch application

## Key types

- `ToolContext` — minimal context: `root: PathBuf` + optional timeout/cancellation + `env: ToolEnv`
- `ToolEnv` — one run's own environment variables (the engine sets `ZDX_HOME`, `ZDX_ARTIFACT_DIR`, `ZDX_THREAD_ID`, `ZDX_MEMORY_ROOT` per turn). Several runs share one process, so these never come from the process environment: every child process a tool starts gets `ctx.env` (`.envs`/`.env`), and path `$VAR` expansion goes through `ctx.resolve_*`/`ToolEnv::expand`, which read the run's value first and fall back to the process environment. The free `expand_env_vars`/`resolve_*` helpers read the process environment only; tool code must not use them.
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
- Anywhere image bytes become base64 bound for a provider, clamp pixel dimensions with `image_downscale::downscale_for_provider` — byte-size caps do not cover it, because a long screenshot compresses small while staying tens of thousands of pixels tall. Vision APIs cap dimensions separately (Anthropic: 8000px, and a stricter cap on every image once a request carries more than 20 image blocks), and an oversized `tool_result` image is rejected outright rather than downscaled server-side, failing the whole turn. Current ingress points: `read.rs`, Telegram photo/document ingest (`zdx-engine` `core/chat_turn.rs` `prepare_inline_image`, called through the service's `prepare_image`, which the Go chat TUI also uses for attachments).
- Image bytes never round-trip through disk (`thread_persistence/replay.rs` drops `ChatContentBlock::Image`), so history replay cannot reintroduce an oversized image. The engine service builds each turn's messages from the thread log (`orchestration/runtime.rs` → `prepare_ordinary_turn`), so its memory is clean by construction; a surface that kept messages in memory across turns would re-send an attached block on every later turn.
- Re-encoding a downscaled image must respect the byte budget the caller already checked the original against, and that bound is hard, not best effort: `downscale_for_provider` prefers JPEG, steps down a quality ladder, then halves the edge limit until the encoding fits. Each step was measured, not assumed: a 2000x1333 noise frame is ~6.4MB as PNG vs ~1.6MB as JPEG, and pure 2000x2000 noise is still ~5.7MB at quality 90. Neither a fixed format nor a fixed quality bounds incompressible content — only dropping pixels does.
