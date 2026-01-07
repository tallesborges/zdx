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
3. User opens command palette (`/` or `Ctrl+P`)
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

## Slice 1: Load Markdown commands from filesystem

- **Goal:** Discover and parse `.md` files from command directories into a data structure
- **Scope checklist:**
  - [ ] Define `CustomCommand` struct: `name`, `description`, `source` (path), `content`, `is_executable`
  - [ ] Implement `load_custom_commands()` → `Vec<CustomCommand>`
  - [ ] Scan `~/.config/zdx/commands/*.md` and `.zdx/commands/*.md`
  - [ ] Parse optional YAML frontmatter for `description`
  - [ ] Filename (without `.md`) becomes command name
  - [ ] Skip files whose names conflict with built-in commands
  - [ ] Add `src/core/commands.rs` with loading logic
- **✅ Demo:** 
  1. Create `~/.config/zdx/commands/test.md` with content "Hello from custom command"
  2. Add temp `main()` code or test that calls `load_custom_commands()` and prints results
  3. Verify: name="test", content="Hello from custom command"
- **Risks / failure modes:**
  - Frontmatter parsing fails on malformed YAML → use permissive parser, treat as no frontmatter
  - Directory doesn't exist → return empty vec, don't error
  - File read fails → skip file, log warning

## Slice 2: Show custom commands in palette

- **Goal:** Custom commands appear in command palette, visually distinguished from built-ins
- **Scope checklist:**
  - [ ] Add `custom_commands: Vec<CustomCommand>` to `TuiState`
  - [ ] Load custom commands at TUI startup (in `TuiRuntime::new()`)
  - [ ] Modify `CommandPaletteState::filtered_commands()` to return merged list
  - [ ] Define enum or wrapper to distinguish built-in vs custom in filtered results
  - [ ] Render custom commands with "(custom)" suffix or different color
- **✅ Demo:**
  1. Create `.zdx/commands/review.md` with frontmatter `description: Review code`
  2. Run `zdx` in that directory
  3. Press `/`, type "rev"
  4. See `/review` with "Review code" description (or "(custom)" if no frontmatter)
- **Risks / failure modes:**
  - Performance on large command dirs → cap at reasonable limit (e.g., 100 commands)
  - Stale commands if files change → acceptable for MVP, reload on `/new` later

## Slice 3: Execute custom Markdown commands (insert content)

- **Goal:** Selecting a custom Markdown command inserts its content into the input field
- **Scope checklist:**
  - [ ] Add `UiEffect::InsertCustomCommand { content: String }` effect
  - [ ] Modify palette's `handle_palette_key()` to return this effect for custom commands
  - [ ] Handle effect in runtime: insert content into `state.input.textarea`
  - [ ] Content replaces any existing input (or appends — decide based on UX)
- **✅ Demo:**
  1. Create `.zdx/commands/explain.md` with "Explain this code step by step:"
  2. Run `zdx`, press `/`, select `/explain`
  3. Input field now contains "Explain this code step by step:"
  4. User can add more text and send
- **Risks / failure modes:**
  - Large content overflows input → textarea handles this, but warn if > 10k chars
  - Content has special chars → should work, textarea handles unicode

## Slice 4: Execute custom executable commands

- **Goal:** Executable files run and their stdout is inserted into input
- **Scope checklist:**
  - [ ] Detect executables: has shebang on first line OR execute bit set
  - [ ] Include executables (no extension required) in `load_custom_commands()`
  - [ ] Add `UiEffect::RunCustomCommand { path: PathBuf }` effect
  - [ ] Handle effect: spawn process, capture stdout+stderr, cap at 50k chars
  - [ ] Insert output into input field (same as Markdown content)
  - [ ] Show error in transcript if execution fails
- **✅ Demo:**
  1. Create `.zdx/commands/staged` with `#!/bin/bash\ngit diff --staged`
  2. `chmod +x .zdx/commands/staged`
  3. Run `zdx`, press `/`, select `/staged`
  4. Input field contains the git diff output
- **Risks / failure modes:**
  - Executable hangs → add timeout (5s default)
  - Executable not found/permission denied → show error in transcript
  - Large output → truncate at 50k with "[truncated]" suffix

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
