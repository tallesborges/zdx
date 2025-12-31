# File Selector Implementation Plan

## Inputs

- **Project/feature**: Add file selector to input - when user types `@` it shows available files to include
- **Existing state**: zdx has Elm-like TUI with reducer pattern, overlays system (model picker, session picker, etc.), tui_textarea for input, effects system for async operations
- **Constraints**: Rust, must fit existing architecture patterns (reducer, effects, overlays)
- **Success looks like**: Type `@` in input, see file list, navigate with arrows, select with Enter/Tab to insert file path

---

# Goals

- Type `@` in input to trigger file picker popup
- Browse workspace files in popup
- Select file to complete the `@` reference (e.g., `@` → `@src/main.rs`)
- Navigate with keyboard (arrows, Enter/Tab, Esc)

# Non-goals

- File content preview
- Multiple file selection
- Directory-only selection mode
- Recursive context injection (inserting file contents automatically)
- Custom file filters beyond gitignore

# Design principles

- **User journey drives order**: ship visible `@` → popup → selection before optimization
- **Leverage existing patterns**: follow overlay pattern (state + update + render) from model_picker
- **Reactive interaction model**: user keeps typing in main textarea, popup passively observes text after `@`
- **Non-blocking**: file discovery via `UiEffect`, not sync in reducer

# User journey

1. User is typing in input textarea
2. User types `@`
3. File picker popup appears above input showing workspace files
4. User navigates with ↑/↓ arrows
5. User types to filter files (fuzzy search)
6. User presses Enter/Tab to select file
7. Selected file path replaces filter text, keeping `@` prefix (e.g., `@mod` → `@src/models.rs`)
8. User presses Esc to dismiss without selection

# Foundations / Already shipped (✅)

## Overlay system
- **What exists**: `OverlayState` enum, overlay handlers (model_picker, session_picker, thinking_picker), render dispatch in view.rs
- **✅ Demo**: Run zdx, press Ctrl+P, see command palette overlay
- **Gaps**: None - pattern is well established

## Input state
- **What exists**: `InputState` with tui_textarea, history navigation, cursor position tracking
- **✅ Demo**: Run zdx, type multi-line input, navigate with arrows
- **Gaps**: No character-level event detection (need to detect `@` typed)

## Effects system
- **What exists**: `UiEffect` enum for async operations, runtime dispatches effects
- **✅ Demo**: Submit message, see `UiEffect::StartAgentTurn` trigger agent loop
- **Gaps**: None for MVP (file walking can be sync initially)

---

# MVP slices (ship-shaped, demoable)

## Slice 1: @ detection and popup trigger

- **Goal**: Type `@` and see a file picker popup appear
- **Scope checklist**:
  - [ ] Add `FilePickerState` struct to `src/ui/chat/overlays/file_picker.rs`
  - [ ] Add `FilePicker(FilePickerState)` variant to `OverlayState` enum
  - [ ] Detect `@` character after keystroke in reducer (check textarea content)
  - [ ] Trigger popup when `@` is typed (reactive: check input text for `@` pattern)
  - [ ] Track `trigger_pos` (byte position of `@` in input)
  - [ ] Add basic key handlers (Esc to close, leaves `@` in input)
  - [ ] Block other overlays while file picker is open
  - [ ] Render empty popup placeholder with border/title
- **✅ Demo**: Run zdx, type `@`, see "File Picker" popup, press Esc to dismiss (with `@` remaining)
- **Risks / failure modes**:
  - Detecting `@` requires post-input inspection of textarea content
  - Need to find `@` position relative to cursor for replacement later

## Slice 2a: Static file list rendering

- **Goal**: Popup renders a hardcoded file list (prove list UI works)
- **Scope checklist**:
  - [ ] Add `files: Vec<String>` to FilePickerState
  - [ ] Add `selected: usize` and `offset: usize` for scroll
  - [ ] Render list with highlight on selected item
  - [ ] Handle ↑/↓ navigation with scroll (clamp to bounds)
  - [ ] Show "loading..." state when files list is empty
- **✅ Demo**: Type `@`, see hardcoded file list, navigate with arrows, scroll works
- **Risks / failure modes**:
  - Scroll offset logic must match session_picker pattern

## Slice 2b: Async file walking

- **Goal**: Popup shows actual files from workspace via async effect
- **Scope checklist**:
  - [ ] Add `ignore` crate dependency (respects gitignore)
  - [ ] Create `src/ui/chat/file_walker.rs` module
  - [ ] Add `UiEffect::LoadFiles` and `UiEvent::FilesLoaded(Vec<String>)`
  - [ ] Implement async file walking in runtime handler (cwd, respect .gitignore)
  - [ ] Limit to first 100 files, max depth 10
  - [ ] Update FilePickerState when files arrive
  - [ ] Handle empty directory (show empty list, no crash)
- **✅ Demo**: Run zdx in project dir, type `@`, see "loading...", then actual file paths
- **Risks / failure modes**:
  - Large directories need depth/count limits
  - Runtime handler must be non-blocking (spawn_blocking or async)

## Slice 3: Selection and text replacement

- **Goal**: Select file to replace filter with path (keeping `@` prefix)
- **Scope checklist**:
  - [ ] Handle Enter/Tab to select current file
  - [ ] On selection: replace text from `trigger_pos+1` to cursor (keeps `@`)
  - [ ] Result: `@filter` becomes `@path/to/file.rs`
  - [ ] Close popup on selection
  - [ ] Add trailing space after path for convenience
- **✅ Demo**: Type `@mod`, press Enter on `src/models.rs`, see `@src/models.rs ` in input
- **Risks / failure modes**:
  - tui_textarea replacement API may require select + delete + insert
  - Cursor position after replacement should be after the trailing space

## Slice 4: Reactive filtering

- **Goal**: Type after `@` to filter file list (reactive model)
- **Scope checklist**:
  - [ ] Extract filter text: substring from `trigger_pos+1` to cursor
  - [ ] Filter files by case-insensitive substring match
  - [ ] Update filtered list on each keystroke (reactive to textarea changes)
  - [ ] Reset selection to 0 when filter changes
  - [ ] Show filter text in popup title: "Files (@filter)"
  - [ ] Handle backspace: if it deletes `@`, close picker
  - [ ] Close picker if cursor moves before `trigger_pos`
- **✅ Demo**: Type `@mod`, see only files containing "mod", backspace to `@`, see all files
- **Risks / failure modes**:
  - Must detect cursor position changes (not just keystrokes)
  - Backspace detection: compare input length before/after

---

# Contracts (guardrails)

1. **Popup dismissed on Esc**: Always closeable; leaves `@` + filter text in input (user can manually delete)
2. **Selection replaces filter only**: Enter/Tab replaces filter text after `@` with path, keeping `@` prefix (e.g., `@mod` → `@src/models.rs`)
3. **Gitignore respected**: Never show files that would be gitignored
4. **No crash on empty dir**: Handle directories with no files gracefully (show empty list)
5. **Input not corrupted**: Selection must cleanly replace `@[filter]` without extra text
6. **Cancel on cursor escape**: Close picker if cursor moves before `@` position
7. **Backspace past `@` closes**: If backspace deletes the `@`, close picker
8. **Overlay exclusivity**: File picker blocks other overlays (Ctrl+P ignored while open)
9. **Non-blocking file walk**: File discovery uses `UiEffect`, reducer never blocks
10. **Multiple `@` handling**: Only the `@` nearest to cursor (and being typed) triggers picker

# Key decisions (decide early)

1. **Trigger character**: `@` (matches reference implementations)
2. **Path format**: Relative paths from cwd (e.g., `src/main.rs`), forward slashes on all platforms
3. **Overlay vs inline**: Use overlay pattern (consistent with model picker) - popup renders above input
4. **Async file walk**: Use `UiEffect` + runtime handler (never block reducer)
5. **Filter matching**: Substring for MVP, fuzzy matching in polish phase
6. **Interaction model**: **Reactive** - user types in textarea, popup observes (no focus trap)
7. **Cancel behavior**: Esc closes popup, leaves `@` + filter in textarea (user deletes manually)
8. **Paths with spaces**: Insert raw path for MVP (no quoting) - `@path/with spaces/file.rs`

# Testing

- **Manual smoke demos per slice**:
  - Slice 1: Type `@`, see popup, Esc dismisses
  - Slice 2a: Popup shows hardcoded list, navigation works
  - Slice 2b: Popup shows real files from cwd
  - Slice 3: Type `@`, select file, see `@src/file.rs ` in input
  - Slice 4: Type `@mod`, see filtered results, select → `@src/models.rs `
- **Minimal regression tests**:
  - Test file walker respects gitignore (unit test with temp dir)
  - Test path insertion replaces `@` correctly (if extraction is complex)

---

# Polish phases (after MVP)

## Phase 1: UX refinement
- [ ] Position popup intelligently (avoid overflow)
- [ ] Truncate long paths with ellipsis
- [ ] Handle hidden files (show by default, respect gitignore)
- **✅ Check-in demo**: Popup positions correctly near edge of terminal

## Phase 2: Fuzzy matching
- [ ] Add fuzzy matching algorithm (or `fuzzy-matcher` crate)
- [ ] Score and sort results by match quality
- [ ] Highlight matched characters in file names
- **✅ Check-in demo**: Type `@mr` and see `main.rs` ranked high

## Phase 3: Performance
- [ ] Add debouncing for filter (wait 50ms after keystroke)
- [ ] Cache file list (invalidate on focus return or timer)
- [ ] Incremental search (don't re-walk on every filter change)
- **✅ Check-in demo**: Large monorepo (>10k files) responds smoothly

---

# Later / Deferred

| Item | Trigger to revisit |
|------|-------------------|
| Multiple file selection | User feedback requesting batch attach |
| File content preview | User feedback requesting preview |
| Custom path prefix (e.g., `@src/`) | User feedback for scoped search |
| Directory-only mode | Feature request for directory operations |
| Tab completion (non-@ trigger) | After @ is stable and proven useful |
| Context injection (insert file contents) | After evaluating token budget implications |
| Quoted paths for spaces | User reports issues with space-containing paths |
| Symlink handling | User reports symlink issues |
| Project root vs cwd toggle | After validating cwd works for most cases |
| File type icons | User feedback requesting visual distinction |
| File count in title | User feedback requesting count |
| "No matches" message | User feedback requesting explicit feedback |

---

## Reference implementations studied

1. **stakpak/agent**: AutoComplete system with async worker, debounced filtering, fuzzy matching
2. **badlogic/pi-mono**: AutocompleteProvider interface, `fd` utility integration, SelectList UI
3. **openai/codex**: ChatComposer + FileSearchManager (debounce/cancel) + FileSearchPopup

All three use `@` trigger, fuzzy matching, gitignore respect, arrow navigation, and Tab/Enter selection.
