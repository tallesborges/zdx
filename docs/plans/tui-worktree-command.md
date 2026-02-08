# TUI `/worktree` Command

# Goals
- User can type `/worktree` in the TUI to create a git worktree for the current thread
- Agent tools (bash, read, write, edit, apply_patch) operate in the worktree directory after switching
- The worktree root is persisted in the thread meta so resuming the thread restores the correct root
- Resuming a worktree-enabled thread restores `agent_opts.root` to the worktree path
- Status line reflects the new path and branch after switching

# Non-goals
- Worktree deletion / cleanup commands (defer)
- Custom worktree naming (use thread ID, matching bot behavior)
- Worktree listing overlay
- Changing the TUI process's actual `cwd` (tools use `ToolContext.root`, not process cwd)

# Design principles
- User journey drives order
- Match existing bot `/worktree` semantics — one worktree per thread, keyed by thread ID
- Follow existing command palette patterns exactly (command → effect → handler → event → mutation)

# User journey
1. User starts a TUI session and opens/creates a thread
2. User types `/worktree` (or selects it from command palette)
3. A git worktree is created at `../.zdx/worktrees/<repo>-<hash>/<thread-id>/` with branch `zdx/<thread-id>`
4. Status line updates to show the worktree path and new branch
5. Transcript shows "Worktree enabled: /path/to/worktree"
6. All subsequent agent turns operate in the worktree directory
7. User quits and later resumes the thread → root is restored to the worktree path automatically

# Foundations / Already shipped (✅)

## Core worktree logic
- What exists: `zdx_core::core::worktree::ensure_worktree()` — finds git root, creates worktree dir, creates/reuses `zdx/{id}` branch
- ✅ Demo: `cargo test -p zdx-core --lib` passes; bot `/worktree` command works
- Gaps: None

## Thread root path persistence
- What exists: `ThreadLog::set_root_path()`, `thread_log::read_thread_root_path()`, `extract_root_path_from_events()`
- ✅ Demo: bot persists and reads root path correctly
- Gaps: TUI thread load has an **auto-relink bug** (see Slice 1)

## Command palette infrastructure
- What exists: `COMMANDS` registry, `execute_command()` dispatch, `UiEffect` → runtime handler pattern
- ✅ Demo: `/new`, `/handoff`, `/rename` all work end-to-end
- Gaps: None — adding a new command is mechanical

## Agent root propagation
- What exists: `TuiState.agent_opts.root` → `spawn_agent_turn()` clones it → `agent::run_turn()` → `ToolContext::new(root)` → all tools use `ctx.root`
- ✅ Demo: bash runs in correct directory, file tools resolve paths relative to root
- Gaps: No mutation exists to update `agent_opts.root` at runtime (only set at startup)

# ⚠️ Discovered bug: thread load clobbers worktree root

`load_thread_sync()` in `runtime/handlers/thread.rs` has auto-relink logic that
**overwrites** the stored worktree root with the TUI's startup directory:

```rust
// Current (broken for worktrees):
if stored_root != current_root {
    handle.set_root_path(root);  // overwrites worktree path with startup dir
}
```

And `handle_thread_loaded()` never updates `agent_opts.root`, so even without the
overwrite, the agent would still run in the wrong directory.

This must be fixed before `/worktree` is useful — otherwise resuming a
worktree-enabled thread silently runs tools in the startup directory.

# MVP slices (ship-shaped, demoable)

## Slice 1: root mutation + fix thread load root restore
- **Goal**: Add the ability to change the agent's working directory at runtime, and fix thread loading to restore a stored root instead of clobbering it
- **Scope checklist**:
  - [x] Add root mutation support (`StateMutation::SetRootDisplay { .. }`) to update `agent_opts.root`, `display_path`, `git_branch`
  - [x] Add `stored_root: Option<PathBuf>` field to `ThreadUiEvent::Loaded`
  - [x] In `load_thread_sync()`: return `stored_root` from events instead of overwriting it. Only write current root if thread had **no** root at all
  - [x] In thread-loaded handling: if `stored_root` is `Some`, emit root refresh effects (`ResolveRootDisplay` + `RefreshSystemPrompt`)
- **✅ Demo**:
  1. Use the bot to create a worktree-enabled thread (or manually set a root via thread meta)
  2. Launch TUI, open that thread via `/threads`
  3. Run `ls` via the agent → output shows the worktree directory, not the startup directory
  4. Status line shows the worktree path and branch
- **Risks / failure modes**:
  - Stored root path no longer exists (worktree deleted) → `SetRoot` points to a missing dir → tools will fail with clear errors. Acceptable for MVP; could add a path-exists check later.
  - Threads created before worktree feature have `stored_root: None` → no mutation emitted, `agent_opts.root` stays at startup dir. Correct behavior.

## Slice 2: `/worktree` command (end-to-end)
- **Goal**: User can type `/worktree` and get a working worktree with agent tools operating there
- **Scope checklist**:
  - [x] Add `"worktree"` entry to `COMMANDS` in `common/commands.rs` (category: `"git"`, aliases: `["wt"]`)
  - [x] Add `UiEffect::EnsureWorktree` variant to `effects.rs`
  - [x] Add `"worktree"` arm in `execute_command()` in `command_palette.rs`:
    - Guard: require active thread (`thread_log.is_some()`)
    - Emit `UiEffect::EnsureWorktree`
  - [x] Handle `UiEffect::EnsureWorktree` in `runtime/mod.rs`:
    - Spawn blocking task with `worktree::ensure_worktree(agent_opts.root, thread_id)`
    - On success: `thread_log.set_root_path(new_path)`
    - Return `UiEvent` with result (new path or error)
  - [x] Add event variants (`ThreadUiEvent::WorktreeReady { path }` / `WorktreeFailed { error }`)
  - [x] Handle event in `update.rs`: trigger root refresh effects + append system message
- **✅ Demo**:
  1. Start TUI, create/open a thread
  2. Type `/worktree` → see "Worktree enabled: /path" in transcript
  3. Send a prompt that uses bash (`ls`) → output shows worktree directory contents
  4. Status line shows new path and `zdx/<thread-id>` branch
  5. Quit and relaunch → open same thread → agent still works in worktree (via Slice 1)
- **Risks / failure modes**:
  - Not inside a git repo → `ensure_worktree` returns error → show as system message (handled by error arm)
  - Already has a worktree → `ensure_worktree` returns existing path (idempotent, no-op effectively)
  - No active thread → guard catches this before effect is emitted

# Contracts (guardrails)
- `ensure_worktree` is idempotent: running `/worktree` twice must not fail or create duplicates
- Thread load must not overwrite a stored worktree root with the startup directory
- Threads without a stored root must continue to use the startup directory (no regression)
- Existing commands must not regress (command palette rendering, filtering, execution)
- Thread root path persistence: resuming a worktree-enabled thread must restore the correct root

# Key decisions (decide early)
- **Worktree ID = thread ID**: matches bot behavior, avoids needing a name input overlay. Decided.
- **No process chdir**: tools use `ToolContext.root` from `agent_opts`. The TUI process stays in the original directory. Decided.
- **Auto-relink inversion**: thread load should adopt the stored root, not overwrite it. Only backfill if thread has no root at all. Decided.

# Testing
- Manual smoke demos per slice
- Existing `cargo test --workspace --lib --tests --bins` must pass (no regressions)
- Slice 1: test that loading a thread with a stored root emits `SetRoot` (could be a unit test on `handle_thread_loaded` if desired)
- Slice 2: manual demo is sufficient — the core logic (`ensure_worktree`) is already tested

## Implementation notes (current)
- Used effect/event split to keep reducer I/O-free:
  - `ResolveRootDisplay { path }` → `RootDisplayResolved { ... }`
  - `RefreshSystemPrompt { path }` → `SystemPromptRefreshed { result }`
- Added path compaction for long status-line paths (first 5 segments + `...` + last 5, with long segment compaction).

# Polish phases (after MVP)

## Phase 1: Thread resume UX
- When loading a thread that has a worktree root, show a subtle indicator in the status line (e.g., 🌳 or `[worktree]`)
- ✅ Check-in demo: switch to a worktree-enabled thread → status line shows indicator

## Phase 2: Worktree health check
- On `SetRoot`, verify the path exists. If not, show a warning and fall back to startup root.
- ✅ Check-in demo: delete a worktree dir, load the thread → warning shown, agent uses startup dir

## Phase 3: Worktree cleanup
- Add `/worktree remove` or `/worktree clean` command to remove worktrees for old threads
- ✅ Check-in demo: `/worktree remove` removes the worktree dir and unregisters from git

# Later / Deferred
- **Custom worktree names**: Would need an input overlay. Revisit if users want multiple worktrees per thread.
- **Worktree listing**: A `/worktree list` subcommand. Revisit if users accumulate many worktrees.
- **Auto-worktree on thread create**: Config option to always create worktrees. Revisit after dogfooding the manual command.
