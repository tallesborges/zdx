# Goals
- Replace the external text-area dependency with an internal, minimal text buffer while keeping user-visible input behavior intact.
- Preserve the existing MVU architecture: state in the model, mutations in the reducer, pure rendering.
- Keep the app daily-usable throughout the transition.

# Non-goals
- Adding new input features or changing keybindings/behavior.
- Redesigning UI layout or rendering logic.
- Broad refactors outside the input path.

# Design principles
- User journey drives order
- Single source of truth for state transitions (model → update → view).
- Low‑drift structures: avoid parallel state that can desync.

# User journey
1. Open the app and focus the input area.
2. Type text, edit it, move the cursor, and insert newlines.
3. Submit the input.

# Foundations / Already shipped (✅)
- What exists: MVU data flow with a dedicated input slice (state/update/render), plus a custom renderer that reads lines + cursor from the input model.
  - ✅ Demo (how to verify quickly): Run the UI and see input rendered while typing.
  - Gaps (only if any): Input state currently relies on an external text-area crate for editing primitives.
- What exists: Input reducer already defines key behavior and submission logic.
  - ✅ Demo (how to verify quickly): Type and submit; key handling works as expected.
  - Gaps (only if any): Key handling delegates to external text-area editing logic.

# MVP slices (ship-shaped, demoable)

## Slice 1: Internal text buffer (core editing)
- Goal: Make the input daily-usable using an internal buffer with core editing and cursor movement.
- Scope checklist:
  - [ ] Define an internal text buffer type (lines + cursor).
  - [ ] Implement core operations: insert text, insert newline, delete, move cursor, read lines/cursor.
  - [ ] Wire buffer into input state and reducer (no external dependency in the input path).
- ✅ Demo: Run the UI; type text, move cursor, delete, add newlines, submit.
- Risks / failure modes:
  - Cursor off-by-one with multi-byte characters.
  - Row/col mapping mismatches affecting rendering.

## Slice 2: Parity for programmatic mutations
- Goal: Restore full parity for programmatic edits used by input flows.
- Scope checklist:
  - [ ] Implement “select all + cut” (or equivalent) used for clear/set operations.
  - [ ] Support set-text + set-cursor mutations without side effects.
  - [ ] Verify history navigation and paste-related flows still work.
- ✅ Demo: Clear input, replace input text, navigate history, and submit.
- Risks / failure modes:
  - Selection state drifting from buffer content.
  - Placeholder/cursor logic breaking due to new buffer semantics.

## Slice 3: Dependency removal + cleanup
- Goal: Fully remove the external crate and clean up integration.
- Scope checklist:
  - [ ] Remove dependency from build config.
  - [ ] Delete/replace any leftover adapter calls.
  - [ ] Ensure build and targeted tests pass.
- ✅ Demo: Build/test without the external crate; run UI and type/edit/submit.
- Risks / failure modes:
  - Hidden transitive usage causing compile errors.
  - Missed references in tests or overlays.

# Contracts (guardrails)
- Input behavior and keybindings must remain consistent for typing, cursor movement, deletion, newline insertion, and submission.
- MVU boundaries must remain intact: reducer owns mutations, view is pure, effects remain explicit.
- No new user-visible features or UI changes.

# Key decisions (decide early)
- Buffer storage strategy (line-based vs. alternative) and cursor indexing semantics.
- How selection/cut is represented to support existing clear/set flows.
- Where to map key events to editing operations (kept in reducer vs. helper).

# Testing
- Manual smoke demos per slice
- Minimal regression tests only for the above contracts

# Polish phases (after MVP)
- Phase 1 (✅ check-in demo): Tighten Unicode/multi-width cursor behavior to match rendering expectations.
- Phase 2 (✅ check-in demo): Performance pass for large inputs (ensure no regressions).

# Later / Deferred
- Full feature parity with the external crate beyond what the app currently uses.
- Any new editing capabilities not already present in today’s behavior.