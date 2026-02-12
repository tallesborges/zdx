# Goals
- Status: Done (2026-02-10)
- Make tool-call parameters visible in the transcript so users can understand what was sent to each tool.
- Show parameter names and values in a readable format during normal usage.
- Keep transcript usability intact while adding parameter visibility.

# Non-goals
- Redesigning the full transcript UI.
- Changing tool execution behavior or tool schemas.
- Improving tool output streaming (separate feature).

# Design principles
- User journey drives order
- Ship a daily-usable summary first, then improve readability/details
- Keep added detail understandable without overwhelming the transcript

# User journey
1. User runs a prompt that triggers one or more tool calls.
2. User sees each tool call with clear argument visibility (not just tool name).
3. User quickly understands what was sent to the tool.
4. User can inspect more detail when needed, without losing scanability.

# Foundations / Already shipped (✅)
List capabilities that already exist and should not be rebuilt.

## Transcript tool-call rendering
- What exists: Tool calls already appear in transcript with state (running/done/error/cancelled) and output previews.
- ✅ Demo: Run a prompt that triggers tools and confirm tool rows render with status and result.
- Gaps: Parameter visibility is partial/inconsistent and not clearly formatted for all tools.

## Tool input data availability
- What exists: Tool input payloads are already available in tool events/cells.
- ✅ Demo: Trigger tool calls and confirm tool inputs are present in runtime/thread data.
- Gaps: Inputs are not consistently surfaced in user-friendly transcript formatting.

## Thread replay path
- What exists: Tool events are rebuilt into transcript cells when reopening a thread.
- ✅ Demo: Reopen a prior thread with tool calls and confirm tool cells are reconstructed.
- Gaps: Replayed tool cells still suffer from limited/unreadable argument display.

# MVP slices (ship-shaped, demoable)
Define Slice 1..N in user-journey order.

## Slice 1: Universal argument summary in tool rows
- **Goal**: Ensure every tool call shows visible arguments immediately in transcript.
- **Scope checklist**:
  - [x] Add a consistent argument summary renderer for tool calls.
  - [x] Show parameter names + values in a compact format for all tools.
  - [x] Keep current tool status visuals unchanged.
- **✅ Demo**: Trigger at least 3 different tools; each tool row shows readable args (not only tool name).
- **Risks / failure modes**:
  - Very long values can clutter rows.
  - Nested payloads may be hard to summarize cleanly.

## Slice 2: Readable detailed formatting for complex args
- **Goal**: Improve readability when arguments are long or nested.
- **Scope checklist**:
  - [x] Add multi-line detail formatting for complex tool args.
  - [x] Preserve clear parameter naming/value boundaries.
  - [x] Apply safe truncation markers for oversized sections.
- **✅ Demo**: Run a tool call with nested/long args and confirm transcript remains readable with clear structure.
- **Risks / failure modes**:
  - Over-formatting may add too much vertical space.
  - Truncation rules may hide useful context if too aggressive.

## Slice 3: Collapsible detail mode (summary-first UX)
- **Goal**: Keep transcript scannable while still allowing deeper inspection.
- **Scope checklist**:
  - [x] Default to compact summary view.
  - [x] Add interaction to expand/collapse detailed args per tool call.
  - [x] Ensure expanded/collapsed rendering works for active and replayed tool cells.
- **✅ Demo**: In one transcript, collapse/expand multiple tool calls and verify summary + detail views are both usable.
- **Risks / failure modes**:
  - Interaction discoverability may be weak.
  - State handling can drift between live and replayed cells.

# Contracts (guardrails)
List non-negotiable behaviors that must not regress (derived from Inputs and existing behavior).
- Tool call state rendering (running/done/error/cancelled) must continue to work.
- Tool result preview behavior must remain intact.
- Argument rendering must work for both live runs and reopened threads.
- Transcript must stay stable with large or malformed argument payloads.

# Key decisions (decide early)
List only decisions that would cause rework if postponed (derived from Inputs).
- Canonical display format for argument summaries (single-line compact structure).
- Thresholds/rules for when to switch from summary to detailed/truncated rendering.
- Default behavior for detail visibility (collapsed-by-default vs expanded-by-default).
- Interaction model for expand/collapse in transcript.

# Testing
- Manual smoke demos per slice
- Minimal regression tests only for contracts

## Current implementation status (2026-02-10)
- Implemented in:
  - `crates/zdx-tui/src/features/transcript/cell.rs`
  - `crates/zdx-tui/src/features/transcript/state.rs`
  - `crates/zdx-tui/src/features/transcript/update.rs`
  - `crates/zdx-tui/src/features/transcript/render.rs`
  - `crates/zdx-tui/src/features/transcript/selection.rs`
- Current UX decisions:
  - First line: `bash` shows command; non-bash shows only tool name.
  - `args:` shown for all non-bash tools.
  - For `bash`, hide `args:` when input is command-only (`{"command": ...}`).
  - `args (json)` disclosure uses AMP-style glyphs (`▶` collapsed, `▼` expanded).
  - Expanded detail uses box-drawing guide (`│`) and output divider (`─ output ─`).
  - Detail cap is `200` lines (KISS for now).

## Review notes to remember (KISS)
- High-priority robustness issue from review (line toggle based on text matching) is addressed via line interaction metadata (`LineInteraction::ToggleToolArgs`).
- Keep current approach simple for now. Revisit only if real lag appears.
- Known medium-risk area (deferred): very large tool args can still incur repeated JSON serialization while tool is running.
  - If this becomes visible in practice, first step is a small optimization pass:
    - short-circuit/early-stop detail row building,
    - optionally cache precomputed args summary/details for running tool cells.

# Polish phases (after MVP)
Group improvements into phases, each with a ✅ check-in demo.
Limited strictly to scope present in Inputs.

## Phase 1: Readability and consistency polish
- Tighten spacing/line-wrapping for parameter blocks.
- Improve consistency of label/value rendering across tools.
- ✅ Check-in demo: Compare before/after transcript screenshots for the same tool calls; readability is clearly improved without losing scan speed.

# Later / Deferred
Explicit list of "not now" items + what would trigger revisiting them.
- Additional transcript UI redesign beyond tool-arg display (revisit if arg UX still feels insufficient after MVP).
- Advanced tool output streaming changes (revisit when "tool calling streaming visualization" becomes active work).