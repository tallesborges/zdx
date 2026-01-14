# Large Paste Placeholder Implementation Plan (v5 - FINAL)

## Goals
- Display condensed placeholders (e.g., `[Pasted Content 1234 chars]`) for large pasted content in the input textarea
- Expand placeholders back to original content on submission
- Keep the UI clean when pasting large content like file paths, code snippets, or long text

## Non-goals
- File path normalization (e.g., `file://` URL parsing, shell escaping) - defer for later
- Custom placeholder character threshold configuration - use hardcoded value first
- Multiple file drag-and-drop handling
- Undo/redo support for placeholder expansion (if user undoes, placeholder sync may be inconsistent)
- Cascade-safe expansion (MVP accepts rare edge case where pasted content contains placeholder text)
- Recovery of pending pastes after failed submission (clearing is intentional trade-off for simplicity)

## Design principles
- User journey drives order
- Ship-first: simple placeholder mechanism before polish
- Existing architecture: follow zdx's Elm/MVU pattern (state → update → render)
- Unambiguous placeholders: use unique IDs to prevent collision with user-typed text

## User journey
1. User pastes large content (>1000 chars) into input textarea
2. Input displays placeholder: `[Pasted Content 1234 chars #1]`
3. User can edit around the placeholder or delete it
4. User presses Enter to submit
5. Placeholder expands to full content before sending to agent
6. **Note**: If submission fails (empty after expansion), pending pastes are cleared - user must re-paste

## Foundations / Already shipped (✅)
### Input state and paste handling
- What exists: `InputState` struct with `textarea`, `handle_paste()` function that sanitizes and inserts text
- ✅ Demo: Paste text into zdx input, verify it appears
- Gaps: No placeholder support for large pastes

### Input mutations
- What exists: `InputMutation` enum for cross-slice state changes
- ✅ Demo: `InputMutation::SetText`, `InputMutation::Clear` work
- Gaps: None

### Submission paths (audit complete)
- `submit_input()` in `update.rs` - main Enter key handler → uses `input.get_text()`
- `build_send_effects()` - receives text from `submit_input()`, creates effects
- Handoff submission - uses `input.get_text()` directly within `submit_input()`
- Bang commands (`!cmd`) - uses trimmed text from `get_text()` within `submit_input()`
- All paths flow through `submit_input()` which calls `input.get_text()` - single point of change ✅
- **Processing order**: expand → trim → parse (trim/parse happens on expanded text)

### Clear/reset triggers (audit complete)
- `InputState::clear()` - called on submit, Esc, Ctrl+C → **clears pending_pastes**
- `InputMutation::Clear` - cross-slice mutation → calls `clear()` → **clears pending_pastes**
- `InputMutation::SetText` - overwrites textarea → **clears pending_pastes** (no way to preserve mapping)
- Thread switch - clears via `InputMutation::Clear` → **clears pending_pastes**
- Handoff submit - calls `clear_history()` and clears transcript → goes through `clear()` → **clears pending_pastes**
- History navigation (`navigate_up/down`) - calls `set_text()` → **clears pending_pastes**
- Draft restore - calls `set_text()` → **clears pending_pastes**

### Textarea mutation paths (audit complete)
All paths that modify textarea content:
1. `handle_main_key()` → `textarea.input(key)` → **sync_pending_pastes() called after**
2. `handle_paste()` → `textarea.insert_str()` → **sync_pending_pastes() called after**
3. `set_text()` → `textarea.select_all() + cut() + insert_str()` → **clear_pending_pastes() called**
4. `clear()` → `textarea.select_all() + cut()` → **clear_pending_pastes() called**
5. `navigate_up/down()` → calls `set_text()` → **clear_pending_pastes() called**
6. `InputMutation::InsertChar` → `textarea.insert_char()` → **sync_pending_pastes() called after**

**No other mutation paths exist.** Verified: no autocomplete, no external inserts in input module.

## MVP slices (ship-shaped, demoable)

### Slice 1: Add pending_pastes storage to InputState ✅
- Goal: Store large paste content mapped to placeholders with unique IDs
- Scope checklist:
  - [x] Add `LARGE_PASTE_CHAR_THRESHOLD` constant (1000)
  - [x] Define `PendingPaste` struct: `{ id: String, placeholder: String, content: String }`
  - [x] Add `pending_pastes: Vec<PendingPaste>` field to `InputState`
  - [x] Add `paste_counter: u32` field to `InputState` for monotonic ID generation
  - [x] Add `clear_pending_pastes()` method - clears vec but keeps counter
  - [x] Wire `clear_pending_pastes()` into `clear()` method
  - [x] Wire `clear_pending_pastes()` into `set_text()` method
  - [x] Add `next_paste_id() -> String` helper: simple incrementing number from counter (1, 2, 3...)
- ✅ Demo: Code compiles, existing tests pass
- Risks / failure modes:
  - State leak if pending_pastes not cleared on new thread → mitigated by clear() hook
  - Counter wraps at 4 billion pastes per session → acceptable, near-impossible to hit

### Slice 2: Create placeholder on large paste ✅
- Goal: Generate and insert placeholder for pastes >1000 chars
- Scope checklist:
  - [x] Add `generate_placeholder(char_count: usize, id: &str) -> String` method
  - [x] Format: `[Pasted Content 1234 chars #1]`
  - [x] Modify `handle_paste()` to detect large pastes (>THRESHOLD chars)
  - [x] For large paste: generate ID, create placeholder, store PendingPaste, insert placeholder
  - [x] For small paste: insert directly (existing behavior)
  - [x] Preserve cursor position after placeholder insertion
- ✅ Demo: Paste >1000 chars, see `[Pasted Content N chars #1]` in input
- Risks / failure modes:
  - Unicode char count: use `text.chars().count()` (Unicode scalar values, not graphemes) - acceptable for MVP
  - Selection behavior: if text selected, placeholder replaces selection (textarea default)

### Slice 3: Expand placeholders on submission ✅
- Goal: Replace placeholders with actual content when user presses Enter
- Scope checklist:
  - [x] Add `get_text_with_pending(&mut self) -> String` method to `InputState` (takes &mut self to clear)
  - [x] Expansion: for each PendingPaste in insertion order, replace ALL occurrences of `placeholder` with `content`
  - [x] Rationale: if user copy-pasted the placeholder, they intended the same content
  - [x] Use single `fold` over pending_pastes with `.replace()` for deterministic order
  - [x] **Always clear pending_pastes after expansion**, even if result is empty/whitespace
  - [x] Rationale: simplifies state management, user can re-paste if needed
  - [x] Update `submit_input()` to use `get_text_with_pending()` instead of `get_text()`
  - [x] Verify: thread log persistence receives expanded text (flows through effects)
- ✅ Demo: Paste large content, submit, verify full content in transcript and sent to agent
- Risks / failure modes:
  - Placeholder in pasted content: unique bracket format with ID makes collision unlikely with natural text
  - Multiple placeholders: iterate in insertion order, replace all occurrences of each
  - Orphaned placeholder (edited by user): appears as literal text in submission - acceptable MVP behavior
  - **Cascade expansion**: If pasted content contains placeholder-like text, it may be modified (see Key Decision #11)

### Slice 4: Handle placeholder deletion/modification ✅
- Goal: Remove pending_paste entry when user deletes or modifies placeholder in textarea
- Scope checklist:
  - [x] Add `sync_pending_pastes()` method: retain only entries whose exact placeholder exists in current textarea text
  - [x] Hook locations (all in `update.rs`):
    - [x] `handle_main_key()` after `textarea.input(key)` - for typing/deletion
    - [x] `handle_paste()` after inserting text - for nested paste edge case
  - [x] Also add to `InputState::apply()` for `InputMutation::InsertChar` case
  - [x] Guard: skip sync if `pending_pastes.is_empty()` (performance)
  - [x] NOT needed in `set_text()` / `clear()` because those already call `clear_pending_pastes()`
- ✅ Demo: Paste large content, delete placeholder, submit, verify no phantom content
- Risks / failure modes:
  - Performance: O(n*m) where n=pending_pastes, m=textarea length - acceptable for small n
  - Partial placeholder edit: if user types inside placeholder, sync removes it → orphaned text remains as literal
  - Performance for long text: MVP accepts O(n*m); can throttle/debounce in polish if needed

## Submission path hookup (where to change)
```
submit_input() in zdx-tui/src/features/input/update.rs:
  - Line: let text = input.get_text();
  + Line: let text = input.get_text_with_pending();
```
Single change point - all submission flows (normal, handoff, bang) go through this.
Processing order: `get_text_with_pending()` → then `trim()` → then parsing happens on expanded text.
Clearing happens inside `get_text_with_pending()` regardless of subsequent submit success/failure.

## Sync hooks (where to call sync_pending_pastes)
```
handle_main_key() in zdx-tui/src/features/input/update.rs:
  After: input.textarea.input(key);
  Add: if !input.pending_pastes.is_empty() { input.sync_pending_pastes(); }
  
handle_paste() in zdx-tui/src/features/input/update.rs:
  After inserting placeholder or text
  Add: if !input.pending_pastes.is_empty() { input.sync_pending_pastes(); }

InputState::apply() in zdx-tui/src/features/input/state.rs:
  In InputMutation::InsertChar arm, after insert_char():
  Add: if !self.pending_pastes.is_empty() { self.sync_pending_pastes(); }
```

## NOT needed hooks (documented decisions)
- `InputMutation::SetText` / `set_text()`: Calls `clear_pending_pastes()` - no sync needed
- `InputMutation::Clear` / `clear()`: Calls `clear_pending_pastes()` - no sync needed
- `navigate_up()` / `navigate_down()`: Calls `set_text()` which clears - no sync needed
- Thread load: Goes through `Clear` or `SetText` mutations - no sync needed

## Orphaned placeholder behavior (explicit decision)
**Scenario**: User pastes large content, gets placeholder, then manually edits the placeholder text (changes char count or ID).

**Behavior**: 
1. `sync_pending_pastes()` detects placeholder no longer matches exactly → removes PendingPaste entry
2. On submission, orphaned placeholder-like text remains as literal text (no expansion)
3. User sees `[Pasted Content 1234 chars #1]` in their message to the agent

**Rationale**: 
- This is acceptable MVP behavior - user intentionally modified the placeholder
- Proactive stripping of placeholder-like patterns risks false positives
- Clear feedback: if you edit it, you own it

## Cascade expansion behavior (explicit decision)
**Scenario**: User pastes content A which contains text that looks like placeholder B's pattern.

**Behavior**: 
1. Sequential `.replace()` over pending_pastes may modify content inside already-expanded text
2. Extremely unlikely in practice: requires pasted content to contain exact placeholder format with matching numeric ID

**Rationale**: 
- Collision requires exact placeholder format match with ID, which is unlikely in natural text
- Single-pass token substitution adds complexity not justified for MVP
- Documented in Non-goals and Key Decision #11
- Polish phase can add position-based expansion if this becomes an issue

## Contracts (guardrails)
1. Pastes ≤1000 chars insert directly (no placeholder)
2. Pastes >1000 chars show placeholder with unique numeric ID in textarea
3. Submitted text contains expanded content, not placeholders (for valid pending pastes)
4. Deleting placeholder removes its pending content mapping
5. Clear/SetText/new thread/thread switch clears pending_pastes
6. Unique ID prevents collision with user-typed text matching placeholder format
7. History navigation clears pending_pastes (no way to restore mapping)
8. All occurrences of a placeholder expand to same content (copy-paste scenario)
9. Edited/orphaned placeholder text appears as literal text in submission (no stripping)
10. `get_text_with_pending()` always clears pending_pastes after expansion (regardless of submit outcome)
11. Cascade expansion through pasted content is accepted for MVP (extremely rare edge case)

## Key decisions (decide early)
1. Threshold value: 1000 chars (matches Codex)
2. Placeholder format: `[Pasted Content N chars #N]` (simple numeric ID)
3. ID generation: Monotonic counter per session starting at 1, wraps at 4B
4. Storage: `Vec<PendingPaste>` struct with id, placeholder, content
5. Expansion timing: on submission (not on render)
6. Char counting: `text.chars().count()` (Unicode scalar values, not graphemes)
7. Replacement: replace ALL occurrences of each placeholder in insertion order (handles copy-paste)
8. Clear behavior: `set_text()` and `clear()` both clear pending_pastes
9. Orphan behavior: edited placeholders appear as literal text (no magic stripping)
10. Submit-abort: pending_pastes cleared even if submission fails (simplicity over recoverability)
11. Cascade risk: accepted for MVP - sequential `.replace()` may modify already-expanded content (unlikely with bracket format)

## Testing
- Manual smoke demos per slice (paste large text, verify placeholder, submit)
- No unit tests for MVP (visual verification sufficient)
- Add regression test after MVP if bugs found

## Polish phases (after MVP)
### Phase 1: Visual feedback ✅
- [x] Style placeholder differently (bold cyan)
- [x] `stylize_with_placeholders()` helper splits text into styled spans
- [x] Placeholders highlighted in both normal and handoff input modes
- ✅ Check-in: Placeholder visually distinct from normal text

### Phase 2: Performance optimization
- Throttle/debounce `sync_pending_pastes()` for long text scenarios
- Only sync on deletion keys (Backspace, Delete, Ctrl+W, etc.) instead of every key
- ✅ Check-in: No lag when typing with pending placeholders

### Phase 3: Undo/redo robustness
- Re-sync pending_pastes on undo/redo if textarea supports it
- ✅ Check-in: Undo after paste, placeholder reappears, content still expands correctly

### Phase 4: Position-based expansion (if cascade becomes issue)
- Track placeholder positions in textarea instead of string matching
- Use single-pass substitution that doesn't modify already-inserted content
- ✅ Check-in: Paste content containing placeholder-like text, verify no cascade corruption

## Later / Deferred
- File path normalization (`file://` URLs, shell escaping) - trigger: user requests
- Configurable threshold - trigger: user feedback on 1000 char limit
- Image/binary paste handling - trigger: multimodal support
- Grapheme cluster counting - trigger: user reports CJK/emoji char count issues
- Orphan placeholder stripping - trigger: user feedback that orphans are confusing
- Submit-abort recovery - trigger: user feedback that losing pastes is frustrating
