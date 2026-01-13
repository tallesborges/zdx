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

## Slice 1: @ detection and popup trigger ✅

- **Goal**: Type `@` and see a file picker popup appear
- **Scope checklist**:
  - [x] Add `FilePickerState` struct to `src/ui/chat/overlays/file_picker.rs`
  - [x] Add `FilePicker(FilePickerState)` variant to `OverlayState` enum
  - [x] Detect `@` character after keystroke in reducer (check textarea content)
  - [x] Trigger popup when `@` is typed (reactive: check input text for `@` pattern)
  - [x] Track `trigger_pos` (byte position of `@` in input)
  - [x] Add basic key handlers (Esc to close, leaves `@` in input)
  - [x] Block other overlays while file picker is open
  - [x] Render popup with border/title
- **✅ Demo**: Run zdx, type `@`, see "Files" popup, press Esc to dismiss (with `@` remaining)

## Slice 2a: Static file list rendering ✅

- **Goal**: Popup renders a file list with navigation
- **Scope checklist**:
  - [x] Add `files: Vec<PathBuf>` to FilePickerState
  - [x] Add `selected: usize` and `offset: usize` for scroll
  - [x] Render list with highlight on selected item
  - [x] Handle ↑/↓ navigation with scroll (clamp to bounds)
  - [x] Show "loading..." state when files are being discovered
- **✅ Demo**: Type `@`, see file list, navigate with arrows, scroll works

## Slice 2b: Async file walking ✅

- **Goal**: Popup shows actual files from workspace via async effect
- **Scope checklist**:
  - [x] Add `ignore` crate dependency (respects gitignore)
  - [x] Implement `discover_files()` in `file_picker.rs`
  - [x] Add `UiEffect::DiscoverFiles` and `UiEvent::FilesDiscovered`
  - [x] Implement async file walking in runtime handler (cwd, respect .gitignore)
  - [x] Limit to 1000 files, max depth 15
  - [x] Update FilePickerState when files arrive
  - [x] Handle empty directory (show empty list, no crash)
- **✅ Demo**: Run zdx in project dir, type `@`, see "loading...", then actual file paths

## Slice 3: Selection and text replacement ✅

- **Goal**: Select file to replace filter with path (keeping `@` prefix)
- **Scope checklist**:
  - [x] Handle Enter/Tab to select current file
  - [x] On selection: replace text from `trigger_pos+1` to cursor (keeps `@`)
  - [x] Result: `@filter` becomes `@path/to/file.rs`
  - [x] Close popup on selection
  - [x] Add trailing space after path for convenience
- **✅ Demo**: Type `@mod`, press Enter on `src/models.rs`, see `@src/models.rs ` in input
- **Implementation notes**:
  - `select_file_and_insert()` in `file_picker.rs` handles the text replacement
  - Cursor is positioned after the trailing space using tui_textarea cursor moves
  - Works with text before and after the `@filter` pattern

## Slice 4: Reactive filtering ✅

- **Goal**: Type after `@` to filter file list (reactive model)
- **Scope checklist**:
  - [x] Extract filter text: substring from `trigger_pos+1` to cursor
  - [x] Filter files by case-insensitive substring match
  - [x] Update filtered list on each keystroke (reactive to textarea changes)
  - [x] Reset selection to 0 when filter changes
  - [x] Show file count in popup title: "Files (N)" (preferred over showing filter)
  - [x] Handle backspace: if it deletes `@`, close picker
  - [x] Close picker if cursor moves before `trigger_pos`
- **✅ Demo**: Type `@mod`, see only files containing "mod", backspace to `@`, see all files
- **Implementation notes**:
  - Filtering was implemented as part of Slices 1-2
  - Title shows count instead of filter text (more useful)

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

## Phase 1: UX refinement (partially done)
- [ ] Position popup intelligently (avoid overflow)
- [x] Truncate long paths with ellipsis (already implemented)
- [x] Handle hidden files (show by default, respect gitignore)
- **✅ Check-in demo**: Popup positions correctly near edge of terminal

## Phase 2: Fuzzy matching ✅
- [x] Add fuzzy matching algorithm (or `fuzzy-matcher` crate)
- [x] Score and sort results by match quality
- [x] Highlight matched characters in file names
- **✅ Check-in demo**: Type `@mr` and see `main.rs` ranked high with "m" and "r" highlighted

## Phase 3: Performance
- [ ] Add debouncing for filter (wait 50ms after keystroke)
- [ ] Cache file list (invalidate on focus return or timer)
- [ ] Incremental search (don't re-walk on every filter change)
- **✅ Check-in demo**: Large monorepo (>10k files) responds smoothly

---

# Later / Deferred

| Item | Status |
|------|--------|
| Multiple file selection | Deferred - User feedback requesting batch attach |
| File content preview | Deferred - User feedback requesting preview |
| Custom path prefix (e.g., `@src/`) | Deferred - User feedback for scoped search |
| Directory-only mode | Deferred - Feature request for directory operations |
| Tab completion (non-@ trigger) | Deferred - After @ is stable and proven useful |
| Context injection (insert file contents) | Deferred - After evaluating token budget implications |
| Quoted paths for spaces | Deferred - User reports issues with space-containing paths |
| Symlink handling | Deferred - User reports symlink issues |
| Project root vs cwd toggle | Deferred - After validating cwd works for most cases |
| File type icons | Deferred - User feedback requesting visual distinction |
| File count in title | ✅ Done - Shows "Files (N)" in title |
| "No matches" message | ✅ Done - Shows "No matches" when filter has no results |

---

## Reference implementations studied

1. **stakpak/agent**: AutoComplete system with async worker, debounced filtering, fuzzy matching
2. **badlogic/pi-mono**: AutocompleteProvider interface, `fd` utility integration, SelectList UI
3. **openai/codex**: ChatComposer + FileSearchManager (debounce/cancel) + FileSearchPopup

All three use `@` trigger, fuzzy matching, gitignore respect, arrow navigation, and Tab/Enter selection.
