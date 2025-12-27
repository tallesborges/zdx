# Custom Commands Implementation Plan

## Project Context

**Feature:** Custom slash commands for ZDX TUI (similar to Amp's custom commands)  
**Existing state:**
- ✅ Slash command infrastructure exists: palette (Ctrl+O), command registry, ExecuteCommand effect, reducer pattern
- ✅ Terminal safety/restore fully implemented (panic hooks, Ctrl+C handling in `src/ui/terminal.rs` and `src/core/interrupt.rs`)
- ✅ Current hardcoded commands: `/config`, `/login`, `/logout`, `/model`, `/new`, `/quit`, `/thinking`

**Constraints:**
- Must use reducer pattern (update → effects → runtime)
- Commands loaded from filesystem (`.zdx/commands` workspace + `~/.config/zdx/commands` user)
- No new dependencies if possible (use std lib for file/process ops)
- YAGNI: defer arguments, reload, complex error handling

**Success looks like:**
- User creates `~/.config/zdx/commands/fixme.md` with content "Find all TODO/FIXME comments in the codebase"
- Opens palette (`Ctrl+O`), types "fix", sees `/fixme` in list, hits Enter
- Prompt input gets the markdown content appended, ready to send
- Same flow works for executable scripts that generate dynamic prompts

---

# Goals

- User can create custom commands (markdown files or executables) in known directories
- Custom commands appear in command palette alongside built-in commands
- Markdown commands append file contents to prompt input
- Executable commands run and append output to prompt input (with size limit)
- Commands survive TUI sessions (no re-register needed)

# Non-goals

- Arguments/parameters to custom commands (add later if needed)
- Hot reload during session (user can restart TUI; add `/reload` later)
- Sandboxing/permission system for executables (YOLO default, consistent with zdx)
- Custom command metadata beyond name+description (no icons, categories, etc.)

# Design principles

- **User journey drives order:** discover → show in palette → select → execute → see output
- **Ship-first:** markdown-only MVP first; executables second
- **Reducer pattern:** loading is an effect; execution returns effects (not direct I/O)
- **KISS/YAGNI:** start with simplest discovery (scan on palette open); defer caching
- **Error tolerance:** missing/unreadable files warn but don't crash palette

# User journey

1. User creates `~/.config/zdx/commands/review.md` with prompt template
2. Opens ZDX interactive mode
3. Presses `Ctrl+O` to open palette
4. Types "rev" to filter
5. Sees `/review` in list with "(custom)" indicator
6. Hits Enter to select
7. Palette closes, prompt input shows the template text
8. User edits if needed, presses Enter to send to agent

# Foundations / Already shipped (✅)

**Terminal safety/restore:**
- ✅ Panic hook installed before TUI setup (`src/ui/terminal.rs::install_panic_hook`)
- ✅ Ctrl+C handler restores terminal (`src/core/interrupt.rs::trigger_ctrl_c`)
- ✅ Normal exit via Drop (TuiRuntime)
- ✅ Demo: `cargo run`, trigger panic/Ctrl+C → terminal restores cleanly

**Slash command infrastructure:**
- ✅ Command palette overlay (`src/ui/overlays/palette.rs`)
- ✅ SlashCommand registry (`src/ui/commands.rs::SLASH_COMMANDS`)
- ✅ ExecuteCommand effect (`src/ui/effects.rs::UiEffect::ExecuteCommand`)
- ✅ Command dispatcher (`src/ui/update.rs::execute_command`)
- ✅ Demo: `cargo run`, press `Ctrl+O`, type "mod", hit Enter → model picker opens

**Gaps:**
- SlashCommand is currently static (`&'static [SlashCommand]`)
- No dynamic command loading mechanism
- No custom command execution logic

# MVP slices (ship-shaped, demoable)

## Slice 1: Markdown command discovery and execution

**Goal:** User can create `.md` files in commands directories; they appear in palette and append to input.

**Scope checklist:**
- [ ] **Architecture refactor (enables dynamic commands):**
  - [ ] Add `CustomCommand` struct with owned `String` fields in `src/ui/commands.rs`
  - [ ] Add `custom_commands: Vec<CustomCommand>` to `TuiState` in `src/ui/state.rs`
  - [ ] Change `UiEffect::ExecuteCommand { name: &'static str }` → `{ name: String }` in `src/ui/effects.rs`
  - [ ] Update `execute_command` in `src/ui/update.rs` to look up both built-in and custom by `String`
  - [ ] Update palette filtering to work with `Vec<&CustomCommand>` instead of static slice
- [ ] **New effect/event plumbing:**
  - [ ] Add `UiEffect::LoadCustomCommands` in `src/ui/effects.rs`
  - [ ] Add `UiEvent::CustomCommandsLoaded { commands: Vec<CustomCommand> }` in `src/ui/events.rs`
  - [ ] Runtime handler in `src/ui/tui.rs`: spawn blocking task, call `discover_custom_commands()`, send event
- [ ] **Discovery logic in new `src/custom_commands.rs`:**
  - [ ] `discover_custom_commands() -> Vec<CustomCommand>` scans:
    - [ ] `.agents/commands` (workspace-local)
    - [ ] `~/.config/zdx/commands` (user-global)
  - [ ] Filters for `.md` files only (executable support deferred to Slice 2)
  - [ ] Returns `CustomCommand { name, path, cmd_type: Markdown, source: Workspace/User }`
  - [ ] Normalize names (lowercase) for collision detection
  - [ ] **Precedence:** built-ins win, then `.agents/commands`, then `~/.config/zdx/commands`
- [ ] **Update reducer (`src/ui/update.rs`):**
  - [ ] On `open_command_palette`: trigger `UiEffect::LoadCustomCommands` if cache stale
  - [ ] On `CustomCommandsLoaded` event: store commands in `state.custom_commands`
  - [ ] On `ExecuteCommand` for custom markdown: read file, append to textarea
  - [ ] Use `String::from_utf8_lossy` for non-UTF8 content (like `src/core/context.rs`)
- [ ] **Update palette (`src/ui/overlays/palette.rs`):**
  - [ ] Merge `SLASH_COMMANDS` + `state.custom_commands` for display
  - [ ] Add `(custom)` suffix to display name
  - [ ] Cache filtered list in palette state (avoid re-merging on every render)
- [ ] **Error handling:**
  - [ ] File not found: show warning in transcript, don't crash
  - [ ] File too large (>50KB): truncate with warning
  - [ ] Non-UTF8: use `from_utf8_lossy`, show warning if lossy conversion happened

**✅ Demo:**
1. `mkdir -p ~/.config/zdx/commands`
2. `echo "Find all TODO comments in the codebase" > ~/.config/zdx/commands/todos.md`
3. `cargo run`
4. Press `Ctrl+O`, type "tod", see `/todos (custom)` in list
5. Hit Enter → prompt input shows "Find all TODO comments in the codebase"
6. Verify: delete file, reopen palette → command gone

**Failure modes / guardrails:**
- Missing directory: silently skip (no error)
- Unreadable file: log warning to transcript, skip command
- File >50KB: truncate to 50KB, append "...(truncated)" marker
- Name collision: built-ins always win, show warning in transcript
- Case-insensitive collision (macOS): normalize to lowercase, first-discovered wins
- Non-UTF8 content: use `String::from_utf8_lossy`, show warning if conversion was lossy

---

## Slice 2: Executable command support

**Goal:** Executables (scripts with shebang or execute bit) run and append stdout/stderr to input.

**Scope checklist:**
- [ ] **Extend discovery (`src/custom_commands.rs`):**
  - [ ] Detect executables: **require execute bit** (Unix) - do NOT rely on shebang alone
  - [ ] Add `CustomCommandType::Executable` variant
  - [ ] Document: scripts without exec bit won't be discovered (explicit UX decision)
- [ ] **New effect/event for async execution:**
  - [ ] Add `UiEffect::RunCustomCommand { path: PathBuf }` in `src/ui/effects.rs`
  - [ ] Add `UiEvent::CustomCommandOutput { name: String, result: Result<String, String> }` in `src/ui/events.rs`
- [ ] **Runtime handler (`src/ui/tui.rs`):**
  - [ ] On `RunCustomCommand`: spawn `tokio::spawn_blocking` (or `tokio::task::spawn_blocking`)
  - [ ] Inside blocking task:
    - [ ] Run `std::process::Command::new(path).output()` with 5s timeout
    - [ ] Capture combined stdout/stderr (max 50KB, truncate with marker)
    - [ ] Send `CustomCommandOutput` event back via channel
  - [ ] Timeout impl: use `tokio::time::timeout` wrapper around blocking call
- [ ] **Update reducer (`src/ui/update.rs`):**
  - [ ] On `ExecuteCommand` for executable: return `UiEffect::RunCustomCommand { path }`
  - [ ] On `CustomCommandOutput` event:
    - [ ] If `Ok(output)`: append to textarea
    - [ ] If `Err(msg)`: show error in transcript, don't append to textarea
- [ ] **UI indicator (optional for MVP):**
  - [ ] Add "Running command..." indicator while waiting for output (or skip for MVP)
- [ ] **Error handling:**
  - [ ] Non-zero exit: treat as error, show stderr in transcript, don't append
  - [ ] Timeout (5s): kill process, show "Command timed out (5s)" in transcript
  - [ ] Spawn failure: show "Failed to execute" in transcript
  - [ ] Output >50KB: truncate to 50KB, append "...(truncated)" marker

**✅ Demo:**
1. Create `~/.config/zdx/commands/branch`:
   ```bash
   #!/bin/bash
   echo "Current branch: $(git branch --show-current)"
   ```
2. `chmod +x ~/.config/zdx/commands/branch`
3. `cargo run`
4. Press `Ctrl+O`, type "branch", see `/branch (custom)` in list
5. Hit Enter → prompt input shows "Current branch: main" (or actual branch)
6. Test timeout: create script with `sleep 10`, verify it times out with warning

**Failure modes / guardrails:**
- Timeout (5s): kill process, show warning "Command '/xyz' timed out (5s limit)"
- Non-zero exit: show stderr in transcript, don't append to textarea
- Output >50KB: truncate to 50KB, append "...(truncated)" marker
- No exec bit: not discovered (require `chmod +x`, document in error message if user tries to add non-exec script)

---

# Contracts (guardrails)

These must NOT regress:

1. **Terminal restore always runs on exit/panic/Ctrl+C** (already shipped, verified in Slice 0)
2. **Command palette remains responsive** - no blocking on slow file I/O (Slice 1: scan is fast enough; Slice 2: executable timeout prevents hang)
3. **Built-in commands always work** - custom command errors don't break palette (error handling per slice)
4. **No stdout pollution** - custom command output goes to textarea only, not terminal (already guaranteed by TUI architecture)
5. **YOLO spirit** - executables run with user's permissions, no sandboxing (consistent with tool execution)
6. **Reducer purity** - custom command loading/execution returns effects or mutates state, never prints (slight exception: Slice 1 mutates textarea directly for simplicity)
7. **Max output size** - custom commands limited to 50KB output (prevents OOM)

# Key decisions (decide early)

## Decision 1: When to scan for custom commands?

**Options:**
- A) On TUI startup (once)
- B) On palette open (every time via effect)
- C) Background watcher (inotify)

**Choice: B (on palette open, cached in state)**  
**Rationale:** Balance between freshness and performance. Trigger `UiEffect::LoadCustomCommands` on palette open, cache results in `TuiState.custom_commands`, mark stale on `/reload`. Avoids re-scanning on every render (palette's `filtered_commands()` is called multiple times). User can create commands mid-session and see them by reopening palette.

## Decision 2: How to execute executables?

**Options:**
- A) Spawn async task, return effect, show spinner
- B) Block on `Command::wait_with_output()` with timeout

**Choice: A (async with result event)**  
**Rationale:** Blocking violates the responsive UI contract—effects run on the TUI runtime loop (`src/ui/tui.rs`), so blocking freezes input/rendering/Ctrl+C. Async aligns with existing patterns (login token exchange, agent tasks). Add `UiEffect::RunCustomCommand` → `tokio::spawn_blocking` → `UiEvent::CustomCommandOutput`.

## Decision 3: Error handling for executable failures?

**Options:**
- A) Append nothing, show error in transcript
- B) Append stdout, show stderr in transcript
- C) Append output with error prefix

**Choice: A (append nothing, show error in transcript)**  
**Rationale:** Clean failure; user knows something went wrong but input isn't polluted. Consistent with "command failed" expectations.

## Decision 4: Command discovery - effect vs direct I/O?

**Options:**
- A) Scan filesystem directly in `open_command_palette()` (I/O in reducer)
- B) Return effect, runtime loads commands, send event back to reducer

**Choice: B (effect → runtime → event)**  
**Rationale:** Preserves reducer purity per zdx architecture (`src/ui/effects.rs`, `src/ui/update.rs`). Add `UiEffect::LoadCustomCommands` → runtime scans → `UiEvent::CustomCommandsLoaded { commands }` → store in `TuiState.custom_commands`. Cache in state; only rescan on palette open or `/reload`.

## Decision 5: Executable detection - exec bit or shebang?

**Options:**
- A) Require exec bit only
- B) Exec bit OR shebang (run via `sh -c` if shebang present)
- C) Shebang only (rely on `#!/usr/bin/env`)

**Choice: A (require exec bit only)**  
**Rationale:** `std::process::Command::new(path)` fails without exec bit on Unix, even if shebang present. Supporting shebang-only would require parsing first line and running via `/bin/sh` explicitly. KISS for MVP: require `chmod +x`. Document clearly: scripts need exec bit to be discovered.

**Options:**
- A) Custom commands shadow built-ins
- B) Built-ins always win
- C) Reject custom command with same name (error at discovery)

**Choice: B (built-ins always win)**  
**Rationale:** Prevents user from accidentally breaking `/quit` etc. Show warning in transcript if collision detected.

# Testing

**Architecture compliance (critical for each slice):**
- Effect/event flow: verify no direct I/O in reducers/overlays
- Command ownership: verify `String` vs `&'static str` usage is consistent
- Async execution: verify UI remains responsive during command execution
- Ctrl+C during custom command: verify terminal restores cleanly

**Manual smoke demos per slice:**

**Slice 1 (Markdown):**
- Create `.md` file → appears in palette → appends to input
- Delete file → disappears from palette
- Large file (>50KB) → truncates with marker
- Unreadable file → shows warning, palette still works
- Name collision → built-in wins, warning shown

**Slice 2 (Executable):**
- Create executable → appears in palette → output appends to input
- Non-executable script with shebang → detected and runs
- Timeout script (`sleep 10`) → times out, shows warning
- Non-zero exit → shows error, no append
- Output >50KB → truncates with marker

**Regression checks (contracts):**
- Ctrl+C during custom command execution → terminal restores cleanly
- Built-in commands still work after custom command errors
- Palette filter works with mixed built-in + custom commands

# Polish phases (after MVP)

## Polish 1: Refinements ✅

**Scope:**
- Add `/reload` command to re-scan custom commands without restarting TUI
- Show custom command file path in palette on hover or in description
- Add `~/.config/zdx/commands/README.md` template on first use
- Config option: `custom_commands_timeout_secs` (default 5)

**✅ Check-in demo:** User changes script, runs `/reload`, sees updated command

## Polish 2: Arguments support ✅

**Scope:**
- Markdown: no-op (defer to later)
- Executable: pass palette filter as `$1` argument
  - Example: user types `Ctrl+O`, `todo src/`, hits Enter → runs `~/commands/todo.sh "src/"`
- Update palette UI to show argument hint for executables

**✅ Check-in demo:** Create `branch.sh` that takes branch name arg, test via palette

## Polish 3: Better errors and metadata ✅

**Scope:**
- Show last-modified timestamp in palette (helps user know which version)
- Persistent error log for failed custom commands (append to `~/.config/zdx/custom_commands.log`)
- Add `description` support: executables can output `# description: <text>` on first line

**✅ Check-in demo:** Create script with description line, verify it shows in palette

# Later / Deferred

**Explicit "not now" items + triggers:**

- **Hot reload / file watching:** Trigger = user feedback "annoying to restart TUI"
- **Sandboxing / permissions:** Trigger = security concern raised; for now, trust user (YOLO)
- **Custom command categories/tags:** Trigger = >20 custom commands, discoverability problem
- **Custom command editor UI:** Trigger = non-technical users request; for now, edit files directly
- **Remote command sources (git repos, URLs):** Trigger = team sharing use case emerges
- **Command history / favorites:** Trigger = user has >10 commands, wants frecency sorting
- **Inline help / examples in palette:** Trigger = documentation debt grows

---

**End of plan. Ready to ship Slice 1.**
