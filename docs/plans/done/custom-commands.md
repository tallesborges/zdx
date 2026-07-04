# Custom Commands Implementation Plan

**Project/feature:** Custom slash commands that users can define as Markdown files (content appended to prompt) or executables (stdout appended to prompt), following Amp's simple design.

**Existing state:**
- Command palette exists (`src/ui/chat/overlays/palette.rs`) with filtering and selection
- Static `COMMANDS` array in `src/ui/chat/commands.rs` defines built-in commands
- `UiEffect::ExecuteCommand` dispatches to `reducer::execute_command()` for execution
- Config paths infrastructure exists (`paths::zdx_home()` → `~/.config/zdx/`)

**Constraints:**
- Follow Amp's design: Markdown OR executables, no placeholder syntax
- Locations: `~/.config/zdx/commands/` (global) and `.zdx/commands/` (project)
- Optional YAML frontmatter for `description` only (for palette display)
- Executables receive arguments, output capped at 50k chars
- Custom commands must not shadow built-in commands (built-ins win)

**Success looks like:** User creates `.zdx/commands/review.md`, opens command palette, sees `/review`, selects it, content appears in input field ready to send.

---

# Goals

- User can create custom commands as Markdown files or executables
- Custom commands appear in command palette alongside built-ins
- Selecting a custom command inserts its content into the prompt input
- Works for both global (`~/.config/zdx/commands/`) and project (`.zdx/commands/`) locations

# Non-goals

- Placeholder expansion (`$1`, `$ARGUMENTS`, etc.) — deferred
- Command arguments in palette UI — deferred
- Streaming executable output — deferred
- MCP/tool integration — out of scope
- Config file command definitions (JSON/TOML) — out of scope

# Design principles

- **User journey drives order**: Get a command from file → palette → input working first
- **Amp's simplicity**: No templating engine, no placeholder syntax in MVP
- **Executable = escape hatch**: Complex logic goes in scripts, not the tool
- **Built-ins win**: Custom commands cannot shadow built-in commands

# User journey

1. User creates `.zdx/commands/review.md` with content "Review this code for bugs..."
2. User opens zdx in that project directory
3. User opens command palette (`/` or `Ctrl+O`)
4. User sees `/review` in the list with description (if frontmatter present) or "(custom)" 
5. User selects `/review`
6. Content "Review this code for bugs..." appears in input field
7. User adds context and sends

# Foundations / Already shipped (✅)

## Command palette overlay
- **What exists:** `CommandPaletteState` with filter, selection, key handling; renders command list
- **✅ Demo:** Run `zdx`, press `/`, see commands, filter by typing, select with Enter
- **Gaps:** Only shows static `COMMANDS` array; needs to merge custom commands

## Built-in commands array
- **What exists:** `COMMANDS: &[Command]` with name, aliases, description
- **✅ Demo:** Check `src/ui/chat/commands.rs`, see 7 commands defined
- **Gaps:** None — will add separate custom commands, not modify this

## Effect system
- **What exists:** `UiEffect::ExecuteCommand { name }` dispatched from palette, handled in runtime
- **✅ Demo:** Select `/new` from palette, thread clears
- **Gaps:** Need new effect type for custom command content insertion

## Config paths
- **What exists:** `paths::zdx_home()` returns `~/.config/zdx/` (or `$ZDX_HOME`)
- **✅ Demo:** `Config::load()` uses this path
- **Gaps:** None

---

# MVP slices (ship-shaped, demoable)

## Slice 1: Load Markdown commands from filesystem ✅ DONE

- **Goal:** Discover and parse `.md` files from command directories into a data structure
- **Scope checklist:**
  - [x] Define `CustomCommand` struct: `name`, `description`, `source` (path), `content`, `is_executable`
  - [x] Implement `load_custom_commands()` → `LoadCustomCommandsResult { commands, warnings }`
  - [x] Scan `$ZDX_HOME/commands/*.md` and `<cwd>/.zdx/commands/*.md`
  - [x] Parse optional YAML frontmatter for `description`
  - [x] Filename (without `.md`) becomes command name
  - [x] Skip files whose names conflict with built-in commands
  - [x] Add `crates/zdx-engine/src/custom_commands.rs` with loading logic
- **Implementation notes / deviations:**
  - Path is `$ZDX_HOME/commands/` (not `~/.config/zdx/commands/`) — matches the actual `paths::zdx_home()` (default `~/.zdx`).
  - Module lives in `crates/zdx-engine/src/custom_commands.rs` (engine, UI-agnostic per `crates/zdx-engine/AGENTS.md`); the workspace has no `src/core/commands.rs`.
  - Loader takes `builtin_names: &[&str]` explicitly so the engine doesn't depend on the TUI's static `COMMANDS` array.
  - Result is `LoadCustomCommandsResult { commands, warnings }` (mirrors the `skills` and `automations` loader patterns) so the caller can surface non-fatal warnings later without panicking.
  - On duplicate name across user/project dirs, **user wins** (loaded first) and a warning is recorded; built-ins always win over both.
  - Malformed/unterminated YAML frontmatter falls back to using the entire file as content (with a warning), per the plan's "permissive parser" guidance.
- **Tests added (in `crates/zdx-engine/src/custom_commands.rs`):**
  - `test_load_custom_commands_empty_dir`
  - `test_load_custom_commands_skips_builtin_names`
  - `test_load_custom_commands_parses_frontmatter`
  - `test_load_custom_commands_no_frontmatter`
  - `test_load_custom_commands_user_and_project_merged`
  - `test_load_custom_commands_user_wins_on_duplicate`
  - `test_load_custom_commands_malformed_frontmatter_falls_back_to_body`
  - `test_load_custom_commands_unterminated_frontmatter_uses_full_body`
  - `test_load_custom_commands_ignores_non_md_files`
  - `test_load_custom_commands_handles_utf8_bom`
- **✅ Demo:** Verified via `cargo test -p zdx-engine custom_commands` — all 15 tests pass; the `parses_frontmatter` and `no_frontmatter` tests cover the plan's smoke demo (file → struct fields).
- **Oracle review applied:**
  - **Bug fix:** `seen_names` is now reserved only after `read_to_string` succeeds, so a failed read on a user file no longer blocks the same-named project file. Covered by `test_load_custom_commands_failed_read_does_not_block_project_file`.
  - Built-in shadowing and duplicate-name detection are now ASCII-case-insensitive (covered by two new tests). The TUI in slice 2 should pass both built-in primary names and aliases to keep the contract tight.
  - Hidden Markdown files (stem starting with `.`) are now skipped with no warning (covered by `test_load_custom_commands_skips_hidden_files`).
  - Body trimming changed from `trim()` to a `trim_blank_envelope` helper that strips leading/trailing blank lines but preserves first-line indentation, so indented prompts (e.g. code blocks) survive intact (covered by `test_load_custom_commands_preserves_first_line_indentation`).
  - Doc comments and the "Absolute path" wording fixed.
- **Forward-looking notes for slice 2 / 4:**
  - Slice 2: when wiring the TUI, pass a `&[&'static str]` containing both `cmd.name` and each `cmd.aliases` so custom commands cannot shadow either.
  - Slice 4: consider replacing `is_executable: bool` with an enum `CustomCommandKind { Markdown { content }, Executable { path } }` to remove the invalid state of `is_executable=true && content=...`.

## Slice 2: Show custom commands in palette ✅ DONE

- **Goal:** Custom commands appear in command palette, visually distinguished from built-ins
- **Scope checklist:**
  - [x] Add `custom_commands: Vec<CustomCommand>` to `AppState` (not `TuiState`) with a builder-style `with_custom_commands(...)` setter
  - [x] Load custom commands at TUI startup in `TuiRuntime::with_history` using `crate::common::commands::builtin_command_identifiers()` (names + aliases)
  - [x] Surface load warnings via `tracing::warn!` (non-fatal)
  - [x] Replace `CommandPaletteState::filtered_commands() -> Vec<&'static Command>` with `filtered_entries() -> Vec<PaletteEntry<'_>>` (enum: `Builtin(&'static Command)` or `Custom(&CustomCommand)`)
  - [x] Render custom commands with category `"custom"` and `(custom)` placeholder description when frontmatter is missing
  - [x] Custom commands match by name + the literal category string `custom`
  - [x] Built-ins always render first; custom entries follow
  - [x] Pass `app.custom_commands.clone()` into `CommandPaletteState::open` from `update.rs`
- **Implementation notes / deviations:**
  - Stored on **`AppState`** (global) rather than per-`TuiState`. Tabs share the list because reload-on-root-change is deferred (see "Polish phase 2"). Keeps `TuiState::with_history` and the 7 `AppState::new` test sites unchanged.
  - Added `pub fn builtin_command_identifiers() -> Vec<&'static str>` in `crates/zdx-tui/src/common/commands.rs` that flattens primary names **and aliases**, so a custom file named `q.md`, `clear.md`, or `wt.md` is correctly skipped (covered by Oracle's slice-1 finding).
  - Selection of a custom entry is a **no-op close** in slice 2; slice 3 will dispatch `InsertCustomCommand`.
  - Custom-entry filtering looks at the name and the literal `custom` category. We intentionally do **not** match against the description text in MVP to avoid surprising behavior; can revisit if users ask.
- **Tests added (in `crates/zdx-tui/src/overlays/command_palette.rs`):**
  - `test_palette_includes_custom_commands_after_builtins`
  - `test_palette_filter_matches_custom_command_name`
  - `test_palette_filter_matches_custom_category`
  - `test_palette_filter_no_match_includes_no_custom`
  - Existing 6 palette tests updated to the new `filtered_entries`/`open(model, customs)` API
  - In `crates/zdx-tui/src/common/commands.rs`: `test_builtin_command_identifiers_includes_names_and_aliases`
- **✅ Demo:**
  1. `.zdx/commands/review.md` already created in repo root with `description: Review code for bugs and clarity` and a body.
  2. `cargo test -p zdx-tui --lib` (258 tests, all green) verifies the palette renders/filters custom entries.
  3. Live demo: `just run`, press `/`, type `rev` → `/review` appears with the description; selecting it closes the palette without effect (slice 3 will insert content).
- **Verification:**
  - `cargo build --workspace` clean.
  - `cargo test -p zdx-engine custom_commands` → 15/15.
  - `cargo test -p zdx-tui --lib` → 258/258.
  - `cargo clippy -p zdx-tui --tests` clean.
- **Oracle review applied:**
  - Added `test_palette_custom_selection_closes_with_no_effects` to pin the slice-2 contract (Enter on a custom entry returns `OverlayTransition::Close` with empty effects/mutations).
  - Confirmed (via git history) that the **claim of an alias-rendering regression was not real**: the original palette also rendered only `cmd.name`, never `cmd.display_name()`. Aliases were always filter-only. No fix needed.
  - Confirmed `tracing::warn!` from the runtime reaches the rolling log file at `$ZDX_HOME/logs/...` even though stderr is taken over by the alternate-screen TUI.
  - Storage on `AppState` (vs per-`TuiState`) is consistent with the plan's "reload at startup only for MVP" decision; tabs sharing the list is intentional.
  - Slice 3 plumbing will be straightforward: the Enter branch on `PaletteEntry::Custom(cmd)` already has direct access to `cmd.content`.

## Slice 3: Execute custom Markdown commands (insert content) ✅ DONE

- **Goal:** Selecting a custom Markdown command inserts its content into the input field
- **Scope checklist:**
  - [x] Decide between new `UiEffect` vs reusing existing `InputMutation::SetText` → reusing existing infrastructure (no new effect needed for Markdown)
  - [x] Modify palette's Enter branch to emit `StateMutation::Input(InputMutation::SetText(content))` for custom entries
  - [x] Confirmed mutation is applied through the standard overlay dispatch path (`update.rs:1202` → `apply_mutations`)
  - [x] Content **replaces** existing input (per Key Decision in plan); cursor lands at end of inserted text (`InputState::set_text` clears + inserts)
- **Implementation notes / deviations:**
  - **No new `UiEffect` variant.** The plan called for `UiEffect::InsertCustomCommand { content: String }`, but the codebase already has `InputMutation::SetText(String)` (`crates/zdx-tui/src/mutations.rs:57`) which the overlay dispatch loop already forwards into `tui.input.apply(mutation)`. Adding a new effect would be pure indirection. Slice 4 still introduces a real `UiEffect::RunCustomCommand` because executables need async I/O.
  - The custom-entry Enter branch now reads:
    ```rust
    PaletteEntry::Custom(cmd) => {
        let content = cmd.content.clone();
        OverlayUpdate::close().with_mutations(vec![StateMutation::Input(
            InputMutation::SetText(content),
        )])
    }
    ```
  - Slice 2's `test_palette_custom_selection_closes_with_no_effects` was renamed to `test_palette_custom_markdown_selection_inserts_content` and now asserts the `SetText` mutation contents.
- **Tests:**
  - `test_palette_custom_markdown_selection_inserts_content` — Enter on filtered custom entry returns `Close` + a single `InputMutation::SetText("…body…")` mutation; effects empty.
  - All previous tests still pass.
- **✅ Demo:**
  1. The repo already has `.zdx/commands/review.md` from slice 2.
  2. `just run` → press `/` → type `rev` → select `/review` → input field is replaced with the review prompt and the cursor is at the end (verified by inspecting `InputState::set_text` + `TextBuffer::insert_str` cursor placement).
  3. Verified via `cargo test -p zdx-tui --lib` (262/262) and `cargo clippy -p zdx-tui --tests` (clean).
- **Oracle review applied:**
  - **Bug fix #1 (handoff active):** Selecting a custom command while `tui.input.handoff.is_active()` would have silently re-routed Enter through handoff submission. Now the palette closes and surfaces a system message ("Cancel the handoff before inserting a custom command.") without touching input. Covered by `test_palette_custom_selection_blocked_during_handoff`.
  - **Bug fix #2 (stale image attachments):** `InputState::set_text` now also calls `self.sync_pending_images()` (mirrors `InputMutation::InsertText`). Without this, images attached to the previous draft would silently survive into the new prompt and be submitted. This was a latent pre-existing bug that history navigation also tripped, but it was made user-visible by slice 3. Two new regression tests in `crates/zdx-tui/src/features/input/state.rs` (`set_text_drops_pending_images_whose_placeholder_is_gone`, `set_text_keeps_pending_images_whose_placeholder_is_preserved`).
  - Confirmed Oracle's recommendation to **not** add `UiEffect::InsertCustomCommand` for Markdown — the existing `StateMutation::Input(InputMutation::SetText(...))` is the correct primitive (file picker uses the same pattern).
  - Forward-looking concern noted: `set_text` still does not call `reset_navigation`, so opening the palette mid-history-navigation can leave history index stale. Acceptable for MVP per Oracle.

## Slice 4: Execute custom executable commands — ⏸ DEFERRED

**Status:** Deferred at user request after slices 1–3 shipped. The user is not
yet sure executables are needed; revisit only when there's a concrete use case.

**Original goal:** Executable files run and their stdout is inserted into input.

**Why deferred:**
- Markdown commands (slices 1–3) already cover the common case ("paste a
  saved prompt") with zero process-spawn risk.
- Executables introduce real complexity: spawn semantics, kill-on-cancel /
  timeout cleanup, output truncation, shebang vs execute-bit detection, and
  a new `UiEffect`/`UiEvent`/`TaskKind` lifecycle. None of that pays off
  unless the user actually wants script-driven prompts.

**State of the codebase:**
- `CustomCommand.is_executable: bool` field is **kept** (always `false` today)
  so re-introducing executable discovery doesn't break the struct API.
- The palette Enter branch has a comment pointing at this deferred slice.
- No `UiEffect::RunCustomCommand`, no `TaskKind::CustomCommand`, no runtime
  handler — all reverted cleanly.

**When to revisit:**
- User explicitly asks for executable commands, OR
- A concrete use case appears (e.g. wanting `/staged` to run `git diff
  --staged`) that's not well served by an interactive bash shortcut.

**If revisited, the existing slice-4 sketch in this file's git history shows
the planned API shape (UiEffect + UiEvent + handler in
`runtime/handlers/custom_command.rs`) and the Oracle review notes that
flagged the must-fix issues (shebang-without-exec-bit detection mismatch,
process kill on cancel/timeout). Use that as the starting point.**

---

# Contracts (guardrails)

1. **Built-in commands always win:** If a custom command has the same name as a built-in, the custom command is ignored
2. **No execution without user action:** Custom commands only run when explicitly selected from palette
3. **Executable output is capped:** Max 50k characters, truncated with indicator
4. **Missing directories are not errors:** Return empty list if command dirs don't exist
5. **Palette remains responsive:** Command loading happens at startup, not on every palette open

---

# Key decisions (decide early)

1. **Content insertion behavior:** Replace input or append?
   - **Decision:** Replace (user can undo with Ctrl+Z if textarea supports it, or clear first is expected behavior)

2. **Executable detection:** Shebang-only or also check execute bit?
   - **Decision:** Both — shebang OR execute bit (like Amp)

3. **Project path for `.zdx/commands/`:** Current working directory or git root?
   - **Decision:** Current working directory (simpler, matches config behavior)

4. **Reload behavior:** When are custom commands reloaded?
   - **Decision:** At startup only for MVP; `/new` can trigger reload in polish phase

---

# Testing

## Manual smoke demos per slice

- **Slice 1:** Create test file, run loader, verify struct fields
- **Slice 2:** Create command, open palette, verify visible and filterable
- **Slice 3:** Select Markdown command, verify content in input
- **Slice 4:** Select executable, verify output in input

## Minimal regression tests

- `test_load_custom_commands_empty_dir` — returns empty vec, no panic
- `test_load_custom_commands_skips_builtin_names` — file named `quit.md` is ignored
- `test_load_custom_commands_parses_frontmatter` — description extracted
- `test_load_custom_commands_no_frontmatter` — content is full file, description is None

---

# Polish phases (after MVP)

## Phase 1: Arguments support
- Pass arguments to executables (e.g., `/review main.rs`)
- Add argument hint display in palette
- **✅ Check-in:** `/staged src/` runs `git diff --staged src/`

## Phase 2: Reload and sync
- Reload commands on `/new` or explicit `/reload` command
- Watch filesystem for changes (optional)
- **✅ Check-in:** Add new command file, run `/reload`, see it in palette

## Phase 3: Better UX
- Show command source path on hover/selection
- Preview command content before executing
- **✅ Check-in:** Select command, see preview in palette

---

# Later / Deferred

| Item | Trigger to revisit |
|------|-------------------|
| Placeholder expansion (`$1`, `$ARGUMENTS`) | User feedback requests it; evaluate complexity vs Amp's "just use executables" approach |
| Config-based command definitions | If users want to define commands without creating files |
| Command aliasing | If users want multiple names for same command |
| Async/streaming executable output | If executables need to show progress |
| MCP tool integration | If custom commands need to invoke tools |
