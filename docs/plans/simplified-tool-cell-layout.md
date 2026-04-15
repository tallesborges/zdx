# Goals
- Replace the verbose multi-line default tool cell with a single compact header: `{icon}  {name}  {key_arg}`
- Open a near-full-screen detail popup when any tool cell is clicked (running, done, error, cancelled)
- Popup shows full args, full output, and live streaming content while the tool runs
- Apply the simplified layout to all tool cells in the main transcript (BTW overlay included, as it shares `display_lines()`)

# Non-goals
- Inline expand/collapse inside the transcript (replaced entirely by the popup)
- Subagent child-tool tree rendering (covered separately in `subagent-tool-activity-tui.md`)
- Changing assistant, user, thinking, or system cell rendering
- Persistent popup state across sessions

# Design principles
- User journey drives order
- Never remove debugging utility: popup must land before or alongside the compact header
- Popup = dedicated detail surface, transcript = scannable summary
- Reuse existing overlay infrastructure (`Overlay` enum, `OverlayRequest`, `OverlayTransition`)

# User journey
1. Agent calls tools → each tool is one compact line: `⟳ Read  crates/foo/bar.rs`
2. Tools complete → `✓ Read  crates/foo/bar.rs`
3. User clicks any tool line → near-full-screen popup opens showing full args + output
4. Tool is still running → popup shows args + live streaming output as it arrives
5. Tool errors → popup shows full error details; main transcript shows `✗` icon inline
6. User presses `q` or `Esc` → popup closes, back to transcript

# Implementation status

## Slice 1: Compact header + tool detail popup ✅

Shipped. All scope items complete.

**What shipped:**
- `tool_key_arg()` extracts key argument per tool type (bash→command, read/write/edit→file_path, etc.)
- `tool_args_expanded`, `tool_output_expanded` removed from `HistoryCell::Tool`
- `toggle_tool_args_expanded`, `toggle_tool_output_expanded` removed
- `ToggleToolArgs`, `ToggleToolOutput` replaced with `OpenToolDetail` in `LineInteraction`
- Tool branch in `display_lines()` rewritten to compact header: `{icon} {name}  {key_arg}`
  - Running: spinner (`◐◓◑◒`), Done: `✓`, Error: `✗`, Cancelled: `⊘`
  - `input_delta` preview rows preserved (write/edit streaming)
  - Error details preserved inline (`Error [{code}]: {message}`)
- `detect_line_interaction` detects tool headers by icon style + ToolStatus span presence
- `ToolDetailState { tool_use_id, scroll_offset, user_scrolled }` in `overlays/tool_detail.rs`
- `ToolDetail(ToolDetailState)` added to `Overlay` enum
- `ToolDetail { tool_use_id }` added to `OverlayRequest`
- Popup: 90% centered, border + title `{icon} {name}`, sections for status/args/output
- Render orchestration in `render.rs` looks up live cell by `tool_use_id`
- Click handler in `transcript/update.rs` opens popup via `OverlayRequest::ToolDetail`
- Cache discriminator updated, dead toggle tests removed
- ~1400 lines of dead code removed (old expand/collapse rendering, unused helpers)
- `completed_at: Option<DateTime<Utc>>` added to `HistoryCell::Tool` for stable duration display

**Files changed:** `cell.rs`, `render.rs`, `selection.rs`, `state.rs`, `update.rs` (transcript), `mod.rs`, `tool_detail.rs` (overlays), `render.rs`, `update.rs` (root TUI), `AGENTS.md`

## Slice 2: Scrollable popup with full output ✅

Shipped. All scope items complete.

**What shipped:**
- Full output rendering (no preview line cap)
- Smart output extraction: stdout/stderr as plain text, file content for read, pretty JSON fallback
- Structured metadata shown alongside stdout/stderr (exit_code, timed_out, stdout_file, etc.)
- Truncation warnings (stdout_truncated, stderr_truncated, file truncated) with `⚠` indicators
- Scroll position indicator `[line/total]` in bottom border
- Scroll bounds computed from wrapped line count (using `Line::width()` + `div_ceil`)
- `u16` overflow protection on scroll offset cast
- `j`/`k`/`↓`/`↑`/`PgUp`/`PgDn`/`g`/`G` navigation

## Slice 3: Live streaming output in popup ✅

Shipped. All scope items complete.

**What shipped:**
- Render-time cell lookup confirmed as providing live data (each frame re-reads from transcript)
- Animated spinner in popup title for running tools (reuses `SPINNER_FRAMES` + `SPINNER_SPEED_DIVISOR`)
- `spinner_frame` passed from render orchestration to popup render
- Auto-scroll to bottom while tool is running (when `!user_scrolled`)
- `user_scrolled: Cell<bool>` — set true on manual scroll keys, set false on `End`/`G` (re-enables auto-follow)
- Scroll position preserved on tool completion (no snap-to-bottom)
- Interior mutability via `Cell<T>` for render-time state (scroll_offset, user_scrolled) since render takes `&AppState`

## Review fixes applied ✅

Code review (oracle) identified and the following were fixed:
- **detect_line_interaction false positives**: error detail lines (Style::ToolError first span) no longer falsely detected as tool headers — now requires both icon style AND ToolStatus span
- **Scroll position preservation**: removed snap-to-bottom on tool completion
- **Stable 'Done' duration**: `completed_at` timestamp used instead of `now - started_at`
- **Wrapped scroll bounds**: line count accounts for text wrapping via `Line::width()`
- **u16 overflow**: saturating cast prevents truncation on very long outputs
- **Metadata visibility**: exit_code, timed_out, stdout_file, etc. shown in popup alongside stdout/stderr
- **Dead code cleanup**: ~540 lines of unused helpers removed from cell.rs

# Contracts (guardrails) — verified ✅
- [x] Error state always visible in main transcript without opening popup (icon + short message inline)
- [x] `input_delta` streaming preview always rendered below header (write/edit tools)
- [x] Lazy rendering (visible range + scroll) does not regress
- [x] BTW overlay renders correctly (shares `display_lines()`) — compact layout inherited automatically
- [x] `cargo test -p zdx-tui` passes (232 tests)
- [x] Affected tests updated/removed with the behavior changes

# Known gaps / follow-ups

## Needs manual verification 🔍
- [ ] Verify `✓`, `✗`, `⊘` glyphs render correctly in development terminal (fallback: ASCII alternatives)
- [ ] Verify BTW overlay compact tool headers look correct visually

## Follow-up work (not in scope for this plan)
- **BTW overlay tool clicks**: BTW gets compact layout via shared `display_lines()`, but its mouse handler doesn't dispatch `OpenToolDetail`. Tool clicks inside BTW don't open the popup.
- **`ToolOutputDelta` streaming**: the `AgentEvent::ToolOutputDelta` event exists in zdx-types but is not yet wired in the transcript update handler. Popup live output for running bash tools depends on this being implemented separately.

# Polish phases (after MVP)

## Phase 1: Keyboard shortcut hint in transcript
- Show a subtle `[click for details]` or `↵` hint on hover / focused tool line
- ✅ Check-in demo: hint visible when navigating to a tool line with keyboard

## Phase 2: Running icon animation
- Decide final animated icon for running state (single `⟳` vs `◐◓◑◒` cycling)
- Currently using `◐◓◑◒` cycling in both transcript and popup title
- ✅ Check-in demo: running tool animates at correct speed

# Later / Deferred
- Subagent child-tool tree inside `invoke_subagent` popup — revisit after this layout is stable (prerequisite)
- Persistent expand state across sessions
- Per-tool-type default popup section (e.g., bash opens straight to output section)
- Copy-to-clipboard button inside popup
- Column-aware click (triangle-only), preserving header text selection — only if whole-line click proves problematic
- BTW overlay tool detail popup support
- `ToolOutputDelta` streaming for live bash output in popup
