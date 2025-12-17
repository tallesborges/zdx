# v0.2.x Implementation Plan

> **Feature:** UX-First + TUI-Ready Core  
> **Goal:** Make the CLI feel excellent now while shaping the core so a TUI becomes "just a renderer."

---

## Assumptions

- The roadmap items will be scoped to what's achievable in incremental commits (not all of v0.2.x in one pass).
- First pass focuses on: streaming UX, system prompts, and AGENTS.md support — the highest ROI items.
- Engine/UI separation will be done incrementally, not as a big-bang refactor.
- Streaming requires `reqwest` with SSE/streaming support (will use `eventsource-stream` or `futures` for chunk reading).
- AGENTS.md caching deferred to later commit (simple include-if-present first).
- Ctrl+C cancellation and tool timeouts are polish items, after core streaming lands.

---

## Summary

Deliver streaming output first (biggest UX win), then system prompt configuration, then AGENTS.md auto-include. Each commit is self-contained and testable. We introduce an internal event enum (`EngineEvent`) early to enable streaming without a full engine extraction — the CLI renders events inline. Tool activity indicators and engine extraction are follow-up commits that build on the event model. Ctrl+C handling comes last as reliability polish.

---

## Commits

### Commit 1: `feat(core): define EngineEvent enum for streaming + future TUI`

**Goal:** Lock the event contract early so streaming and future TUI share the same model.

**Deliverable:** A new `src/events.rs` module with `EngineEvent` enum. No runtime behavior change yet.

**CLI demo command(s):**
```bash
cargo build
cargo test --lib
```

**Expected output:** Compiles, tests pass.

**Files changed:**
- create `src/events.rs` — define `EngineEvent` enum with variants: `AssistantDelta { text: String }`, `AssistantFinal { text: String }`, `ToolStarted { id, name }`, `ToolFinished { id, result }`, `Error { message }`, `Interrupted`
- modify `src/lib.rs` — add `pub mod events;`

**Tests added/updated:**
- `src/events.rs` — unit test that events serialize/deserialize correctly (round-trip)

**Edge cases handled:**
- None (contract definition only)

**Notes:**
- Minimal: just types, no integration yet.
- Variants are additive; we can extend later.
- Serialization supports future JSON output mode.

---

### Commit 2: `feat(provider): add streaming support to Anthropic client`

**Goal:** Enable chunk-by-chunk token streaming from Anthropic API.

**Deliverable:** `AnthropicClient::send_messages_stream()` returns an async stream of `StreamEvent` (delta text, tool_use start, etc.).

**CLI demo command(s):**
```bash
cargo build
cargo test --lib
```

**Expected output:** Compiles. Unit test for stream parsing passes (uses recorded SSE fixture).

**Files changed:**
- modify `Cargo.toml` — add `futures-util` (for `Stream` trait), `tokio-util` if needed for codec
- modify `src/providers/anthropic.rs` — add `send_messages_stream()` method, add internal `StreamEvent` enum, SSE line parser

**Tests added/updated:**
- `src/providers/anthropic.rs` — unit test: parse a hardcoded SSE chunk sequence into expected `StreamEvent` values (no network)

**Edge cases handled:**
- Incomplete SSE line buffering
- `[DONE]` sentinel handling
- Error event from API mid-stream

**Notes:**
- Does not change existing `send_messages()` — additive.
- Streaming is opt-in; non-streaming path unchanged.
- No wiremock streaming test yet (complex); unit test with fixtures is sufficient.

---

### Commit 3: `feat(cli): stream assistant text to stdout in exec`

**Goal:** User sees tokens as they arrive instead of waiting for full response.

**Deliverable:** `zdx exec -p "..."` prints text incrementally. Final newline after response completes.

**CLI demo command(s):**
```bash
ANTHROPIC_API_KEY=... zdx exec -p "Write a haiku about Rust"
# tokens appear one-by-one
```

**Expected output:** Haiku streams to terminal; cursor advances with each chunk.

**Files changed:**
- modify `src/agent.rs` — add `execute_prompt_streaming()` that uses new stream API, emits `EngineEvent::AssistantDelta` internally, flushes stdout per delta
- modify `src/main.rs` — call streaming variant in `run_exec`

**Tests added/updated:**
- `tests/exec_mock.rs` — add streaming mock test: wiremock returns SSE response, assert final output matches expected text (order preserved)

**Edge cases handled:**
- Tool use mid-stream (pause streaming, execute tool, resume)
- Empty delta events (skip printing)
- API error mid-stream (print error, exit non-zero)

**Notes:**
- Chat mode streaming is separate commit to keep diffs small.
- Session logging still logs final text (not deltas) — unchanged.

---

### Commit 4: `feat(cli): stream assistant text in chat mode`

**Goal:** Interactive chat also streams responses.

**Deliverable:** Chat REPL streams assistant output; prompt returns after streaming completes.

**CLI demo command(s):**
```bash
ANTHROPIC_API_KEY=... zdx
you> Tell me a joke
# streams response
assistant> ...
```

**Expected output:** Tokens stream inline, then prompt reappears.

**Files changed:**
- modify `src/chat.rs` — refactor inner loop to use `execute_prompt_streaming()` or inline stream consumption, flush per delta

**Tests added/updated:**
- `tests/chat_mock.rs` — add streaming chat test with SSE mock

**Edge cases handled:**
- User Ctrl+C during stream (graceful, no panic) — basic handling; full cancellation in later commit
- Tool use during chat stream

**Notes:**
- Shares streaming infra from commit 3.
- Session logging unchanged (final text only).

---

### Commit 5: `feat(cli): add tool activity indicator during execution`

**Goal:** User sees which tool is running instead of silent wait.

**Deliverable:** One-line status printed when tool starts: `⚙ Running bash…` / `⚙ Reading file…`. Cleared/overwritten when tool finishes.

**CLI demo command(s):**
```bash
ANTHROPIC_API_KEY=... zdx exec -p "list files in current directory"
# shows: ⚙ Running bash…
# then shows result
```

**Expected output:** Status line appears during tool execution, disappears after.

**Files changed:**
- modify `src/agent.rs` — emit `EngineEvent::ToolStarted` / `ToolFinished`, print status line to stderr
- modify `src/chat.rs` — same indicator logic

**Tests added/updated:**
- `tests/tool_use_loop.rs` — assert stderr contains tool indicator text during tool use mock

**Edge cases handled:**
- Multiple sequential tools (indicator updates per tool)
- Tool error (indicator still cleared)

**Notes:**
- Indicator goes to stderr so stdout remains clean for piping.
- No spinner (complexity); static text is sufficient for now.

---

### Commit 6: `feat(config): add system_prompt and system_prompt_file to config`

**Goal:** User can customize system prompt via config file.

**Deliverable:** Config supports `system_prompt = "..."` (inline) or `system_prompt_file = "path"` (file reference). File takes precedence if both set.

**CLI demo command(s):**
```bash
echo 'system_prompt = "You are a Rust expert."' >> ~/.config/zdx/config.toml
zdx exec -p "explain ownership" --no-save
# assistant uses custom system prompt
```

**Expected output:** Response reflects system prompt personality.

**Files changed:**
- modify `src/config.rs` — add `system_prompt: Option<String>`, `system_prompt_file: Option<String>`, add `Config::effective_system_prompt()` method
- modify `src/agent.rs` — pass system prompt to API request
- modify `src/providers/anthropic.rs` — add `system` field to `MessagesRequest`

**Tests added/updated:**
- `src/config.rs` — unit tests for system_prompt loading (inline, file, precedence)
- `tests/exec_mock.rs` — integration test: config with system_prompt, assert request body contains it

**Edge cases handled:**
- File not found (error with clear message)
- Empty system prompt (treated as no system prompt)
- Both set (file wins)

**Notes:**
- No CLI flag yet (next commit).
- Minimal change to provider module.

---

### Commit 7: `feat(cli): add --system-prompt flag to exec and chat`

**Goal:** Override system prompt from command line.

**Deliverable:** `--system-prompt "..."` flag on `exec` and default chat command.

**CLI demo command(s):**
```bash
zdx exec -p "hello" --system-prompt "Respond only in haiku" --no-save
```

**Expected output:** Response is a haiku.

**Files changed:**
- modify `src/cli.rs` — add `--system-prompt` to `Cli` (global) or to `Exec` and default
- modify `src/main.rs` — pass CLI override to agent, merge with config (CLI wins)

**Tests added/updated:**
- `tests/exec_mock.rs` — test `--system-prompt` flag overrides config

**Edge cases handled:**
- Flag + config both set (flag wins)
- Empty string flag (clears system prompt)

**Notes:**
- Profile support (`--profile`) deferred to later commit.
- Flag is consistent across exec/chat.

---

### Commit 8: `feat(context): auto-include AGENTS.md in system prompt`

**Goal:** Project conventions automatically included when AGENTS.md present.

**Deliverable:** If `AGENTS.md` exists in `--root` directory, its contents are appended to system prompt.

**CLI demo command(s):**
```bash
echo "# Project Guidelines\nUse snake_case." > AGENTS.md
zdx exec -p "write a function name" --no-save
# assistant follows AGENTS.md conventions
```

**Expected output:** Response uses snake_case.

**Files changed:**
- create `src/context.rs` — `load_project_context(root: &Path) -> Option<String>` that reads AGENTS.md if present
- modify `src/lib.rs` — add `pub mod context;`
- modify `src/agent.rs` — call `load_project_context`, append to system prompt

**Tests added/updated:**
- `src/context.rs` — unit test: AGENTS.md present returns content; absent returns None
- `tests/exec_mock.rs` — integration test with temp AGENTS.md file, assert system prompt contains its content

**Edge cases handled:**
- AGENTS.md not found (no error, just skip)
- AGENTS.md unreadable (warning to stderr, continue without)
- Large AGENTS.md (read up to 50KB, truncate with warning)

**Notes:**
- No caching yet (deferred).
- Simple concatenation: system_prompt + "\n\n" + AGENTS.md content.

---

### Commit 9: `feat(reliability): handle Ctrl+C gracefully`

**Goal:** Ctrl+C during execution leaves session in consistent state.

**Deliverable:** Ctrl+C triggers graceful shutdown: logs `Interrupted` event to session, prints message, exits cleanly.

**CLI demo command(s):**
```bash
zdx exec -p "write a long story" &
# press Ctrl+C
# session file contains interrupted event
```

**Expected output:** `^C Interrupted.` printed, session has `{"type":"interrupted",...}` event.

**Files changed:**
- modify `src/session.rs` — add `SessionEvent::interrupted()` constructor
- modify `src/main.rs` — install `ctrlc` handler (add `ctrlc` crate), set atomic flag
- modify `src/agent.rs` — check interrupt flag in streaming loop, emit `EngineEvent::Interrupted`
- modify `src/chat.rs` — same interrupt handling

**Tests added/updated:**
- `src/session.rs` — unit test for interrupted event serialization
- Manual test documented in commit message (Ctrl+C is hard to automate)

**Edge cases handled:**
- Interrupt during tool execution (tool may complete, but no further API calls)
- Interrupt during streaming (stop consuming stream, log partial response)
- Double Ctrl+C (force exit)

**Notes:**
- Adds `ctrlc` crate (small, well-maintained).
- No timeout handling yet (separate concern).

---

### Commit 10: `feat(tools): add configurable tool timeout`

**Goal:** Prevent stuck tool executions from blocking indefinitely.

**Deliverable:** Config option `tool_timeout_secs = 30` (default). Tools that exceed timeout return error.

**CLI demo command(s):**
```bash
echo 'tool_timeout_secs = 5' >> ~/.config/zdx/config.toml
zdx exec -p "run: sleep 10" --no-save
# tool times out after 5s
```

**Expected output:** Tool returns error: `Tool execution timed out after 5 seconds`.

**Files changed:**
- modify `src/config.rs` — add `tool_timeout_secs: u32` with default 30
- modify `src/tools/mod.rs` — wrap `execute_tool` in `tokio::time::timeout`
- modify `src/tools/bash.rs` — ensure async-compatible execution

**Tests added/updated:**
- `src/tools/mod.rs` — unit test with mock slow tool (sleep), assert timeout error
- `tests/tool_bash.rs` — integration test with short timeout config

**Edge cases handled:**
- Timeout = 0 (disable timeout, infinite wait)
- Bash command ignores SIGTERM (force kill after grace period — stretch goal)

**Notes:**
- Bash tool may need refactor to async spawn for proper timeout.
- Keep simple: timeout wrapper around existing sync execution.

---

## Definition of Done

- [ ] All 10 commits merged to main
- [ ] `cargo test` passes (unit + integration)
- [ ] `cargo clippy -- -D warnings` clean
- [ ] `cargo fmt --check` clean
- [ ] Manual smoke test: `zdx exec -p "hello"` streams response
- [ ] Manual smoke test: `zdx` chat mode streams responses
- [ ] Manual smoke test: AGENTS.md auto-included
- [ ] Manual smoke test: Ctrl+C gracefully interrupts
- [ ] README updated with new config options (system_prompt, tool_timeout_secs)

---

## Next Safe Refactors (Post v0.2)

1. **Extract engine crate** — Move agent loop + event emission to `zdx-core` crate. CLI becomes thin renderer. Enables TUI to share core.

2. **System prompt profiles** — `--profile rust-expert` loads from `~/.config/zdx/profiles/rust-expert.md`. Config can set default profile.

3. **AGENTS.md caching** — Memoize content + mtime per session to avoid repeated reads. Add `context_cache` to session state.

4. **Improved transcript formatting** — Color-coded blocks, tool output collapsing, timestamps. Requires terminal capability detection.

5. **Completion subcommand** — `zdx completion bash > ~/.bashrc` for shell completions. Use clap's built-in generator.
