# Tabs: Replace BTW Overlay with Full-Screen Tab System

# Goals
- Replace the BTW popup overlay with a full-screen tab system
- `/btw` creates a new tab forked from the current thread context
- Users can navigate between tabs and see a tab indicator
- Each tab behaves like a normal chat session (full input, transcript, agent)
- Background agent work continues in non-active tabs

# Non-goals
- Tab persistence across app restarts (tabs are session-scoped)
- Drag-and-drop tab reordering
- Splitting the screen to show multiple tabs simultaneously
- Merging tab content back into the main thread
- More than ~5 concurrent tabs (no perf optimization needed)

# Design principles
- User journey drives order
- Ship ugly-but-functional tab bar before polishing
- Stable `TabId` for async routing — never route by vector index
- Extract the shared/tab-local boundary carefully before adding multi-tab

# User journey
1. User opens zdx — sees the normal chat (tab bar shows `[main]`)
2. Mid-conversation, user types `/btw` — a new full-screen tab opens with forked context
3. User asks a side question in the new tab — agent responds normally
4. User switches back to main tab — main thread is untouched
5. User sees tab indicator showing activity in the btw tab
6. User closes the btw tab when done — returns to main

# Foundations / Already shipped (✅)

## BTW overlay side-chat
- What exists: `BtwState` in `crates/zdx-tui/src/overlays/btw.rs` — independent `TranscriptState`, `AgentState`, `TextBuffer`, `thread_handle`, `messages`, with its own model/thinking overrides
- ✅ Demo: `/btw` opens a popup, type a question, get an agent response
- Gaps: Cramped 96×28 popup, can only have one at a time, separate code path from main chat

## Split-state architecture
- What exists: `AppState { tui, overlay }` allows simultaneous `&mut tui` + `&mut overlay` borrows (`state.rs:53-56`)
- ✅ Demo: Agent can stream in BTW overlay while main thread state is preserved
- Gaps: Hardcoded two-route event system (`UiEvent::Agent` vs `UiEvent::BtwAgent`)

## Thread persistence
- What exists: `ThreadState` with `thread_handle: Option<Thread>`, thread creation via `tp::Thread::new_with_root()` (`runtime/handlers/thread.rs:416`)
- ✅ Demo: Threads persist to disk with messages and metadata
- Gaps: BTW already creates persistent threads — need to decide if btw tabs should too (yes, keep current behavior)

## Command palette / slash commands
- What exists: `/btw` triggers via command palette (`common/commands.rs:45`, `overlays/command_palette.rs:120`)
- ✅ Demo: Type `/btw` → command palette opens → btw selected
- Gaps: None for this feature

# MVP slices (ship-shaped, demoable)

## Slice 1: Extract TabState from TuiState (foundation refactor)

- **Goal**: Split `TuiState` into `TabState` (per-conversation) + `SharedState` (global), keeping a single tab. Everything works identically to today.
- **Scope checklist**:
  - [ ] Define `TabId` (newtype over `u64` or UUID) in `state.rs`
  - [ ] Define `TabKind` enum: `Main`, `Btw { base_messages: Vec<ChatMessage> }`
  - [ ] Define `TabState` struct with: `id`, `kind`, `input`, `transcript`, `thread`, `agent_state`, `status_line`, `input_area`, `system_prompt`, `agent_opts`
  - [ ] Define `SharedState` struct with: `config`, `tasks`, `auth`, `git_branch`, `display_path`, `optimistic_active_threads`, `base_model`, `base_thinking_level`
  - [ ] Restructure `AppState` to: `tabs: Vec<TabState>`, `active_tab: usize`, `shared: SharedState`, `overlay: Option<Overlay>`
  - [ ] Add `AppState::split_active_mut() -> (&mut TabState, &mut SharedState, &mut Option<Overlay>)` helper for safe borrowing
  - [ ] Add `AppState::active_tab() -> &TabState` and `active_tab_mut() -> &mut TabState` convenience methods
  - [ ] Initialize with single `tabs[0]` = main tab from existing `TuiState` fields
  - [ ] Update `update.rs` — replace `app.tui.*` references with `tab.*` via split helper
  - [ ] Update `render.rs` — render from active tab + shared state
  - [ ] Update `runtime/mod.rs` — read/write state through the new structure
  - [ ] Update all feature modules (`input/`, `thread/`, `transcript/`) to work with new field paths
  - [ ] Keep BTW overlay working as-is during this slice (it still uses `Overlay::Btw`)
- **✅ Demo**: `just ci` passes. App launches, chat works, `/btw` overlay still works. No user-visible change.
- **Risks / failure modes**:
  - Wrong shared/tab-local boundary — `status_line`, `agent_opts`, `system_prompt` must be tab-local (oracle confirmed). `config` effective model/thinking mutations must be checked.
  - Large mechanical change — many `app.tui.X` → `tab.X` replacements. Mitigate by using the split helper pattern and doing it file-by-file with compile checks.

## Slice 2: Tab bar rendering + tab-aware events

- **Goal**: Render a tab bar showing tabs. Introduce `TabId`-based event routing (replacing hardcoded `BtwAgent` events).
- **Scope checklist**:
  - [ ] Add tab bar to the render layout — thin line at the top or integrated into status bar showing `[main]`
  - [ ] Highlight the active tab, show tab count
  - [ ] Define `UiEvent::TabAgent { tab_id, event }` and `UiEvent::TabAgentSpawned { tab_id, ... }` — replacing `BtwAgent`/`BtwAgentSpawned`
  - [ ] Define `UiEffect::StartTabTurn { tab_id, ... }` — replacing `StartBtwTurn`
  - [ ] Route `TabAgent` events to the correct tab by `TabId` lookup (not index)
  - [ ] Main thread agent events also use the tab routing (unify `Agent`/`BtwAgent` into one path)
  - [ ] Update `Tick` handler to coalesce pending deltas for all tabs (not just active)
- **✅ Demo**: Tab bar shows `[main]`. Agent turns still work. Events route correctly through the new unified path.
- **Risks / failure modes**:
  - Breaking main-thread agent streaming during event unification. Mitigate by keeping both old + new event paths temporarily if needed, removing old paths after verification.

## Slice 3: /btw creates a new tab

- **Goal**: `/btw` creates a full-screen tab (forked from main context) instead of opening a popup overlay.
- **Scope checklist**:
  - [ ] Change `OverlayRequest::Btw` handling to push a new `TabState` with `kind: TabKind::Btw { base_messages }` instead of creating `Overlay::Btw`
  - [ ] New tab captures `base_messages` from the current active tab's thread (same fork logic as current `BtwState::open`)
  - [ ] Switch `active_tab` to the new tab
  - [ ] New tab gets its own `InputState`, `TranscriptState`, empty `ThreadState` (thread created on first send)
  - [ ] Tab bar updates to show `[main] [btw 1]`
  - [ ] Agent turn in btw tab uses `StartTabTurn` with the tab's `TabId`
  - [ ] The btw tab's agent turn prepends `base_messages` as context (same as current btw behavior)
- **✅ Demo**: `/btw` opens a full-screen tab. Type a question → agent responds. Tab bar shows two tabs.
- **Risks / failure modes**:
  - Thread creation for btw tabs — need to create a thread handle on first send (via `UiEffect::CreateThread` or inline). Current btw does this in `prepare_btw_turn`.

## Slice 4: Tab navigation

- **Goal**: Users can switch between tabs with keyboard shortcuts.
- **Scope checklist**:
  - [ ] Add keybinding: `Ctrl+PageUp` / `Ctrl+PageDown` to cycle tabs (or `Alt+Left`/`Alt+Right` — check terminal compatibility)
  - [ ] Add keybinding: `Ctrl+N` (or another available key) as alias for `/btw` to create new tab quickly
  - [ ] Add `/tabs` slash command to show tab list in command palette (pick to switch)
  - [ ] Tab switching preserves each tab's input buffer, scroll position, and agent state
  - [ ] Visual: active tab highlighted in tab bar, non-active tabs show name only
- **✅ Demo**: Create 2 btw tabs. Switch between all 3 tabs. Each preserves its state. Type partial input in tab 1, switch to tab 2, switch back — input preserved.
- **Risks / failure modes**:
  - Some terminal emulators don't send `Ctrl+PageUp/Down`. Provide a slash-command fallback (`/tabs`).
  - `input_area` cache (`render.rs:87-88`) and mouse click routing (`update.rs:923,930-936`) need to work with the active tab's layout, not a stale cached value.

## Slice 5: Background activity indicators + close tabs

- **Goal**: Show activity in background tabs. Allow closing btw tabs.
- **Scope checklist**:
  - [ ] Tab bar shows activity indicator (e.g. `*` or spinner) on tabs with running agents
  - [ ] Tab bar shows unread indicator on tabs that received new content while not active
  - [ ] `Esc` in an idle btw tab (or `/close`) closes it and switches to the previous tab
  - [ ] Closing a tab with a running agent: cancel the agent first, then close
  - [ ] Prevent closing the main tab (tab 0)
  - [ ] When a tab is closed, route to the nearest remaining tab
- **✅ Demo**: Start agent in btw tab → switch to main → see `*` indicator on btw tab → switch back → see response → `/close` → back to main.
- **Risks / failure modes**:
  - Closing a tab while its agent is streaming: must cancel the agent task and clean up the receiver. Current interrupt logic (`UiEffect::InterruptAgent`) needs to be tab-aware.
  - Stale `TabId` references: after closing a tab, any pending events for that `TabId` must be silently dropped.

## Slice 6: Remove old BTW overlay code

- **Goal**: Clean up dead code from the overlay-based BTW implementation.
- **Scope checklist**:
  - [ ] Remove `Overlay::Btw` variant from the `Overlay` enum (`overlays/mod.rs`)
  - [ ] Remove `BtwState` struct and all btw overlay code (`overlays/btw.rs`)
  - [ ] Remove `UiEvent::BtwAgent`, `UiEvent::BtwAgentSpawned` if not already removed in Slice 2
  - [ ] Remove `UiEffect::StartBtwTurn` if not already removed in Slice 2
  - [ ] Remove btw-specific runtime handlers (`runtime/handlers/thread.rs` btw functions)
  - [ ] Remove btw overlay rendering (`render_btw_overlay`)
  - [ ] Update `crates/zdx-tui/AGENTS.md` to reflect the new tab architecture
- **✅ Demo**: `just ci` passes. `rg -i "btw" crates/zdx-tui/src/overlays/btw.rs` — file doesn't exist. No dead code.
- **Risks / failure modes**:
  - Missing a reference to old btw code — compiler will catch it.

# Contracts (guardrails)
- Main tab (tab 0) cannot be closed
- `/btw` must fork the active tab's conversation context as base messages (existing contract)
- Agent streaming in a background tab must continue uninterrupted when switching tabs
- Each tab's input buffer, scroll position, and thread state are independent
- Tab navigation keybindings must not conflict with existing `Ctrl+A/E/U/K/J/W/C/O/L/T` bindings (`input/update.rs`)
- All non-btw overlays (command palette, model picker, thread picker, etc.) continue to work as before

# Key decisions (decide early)
- **Tab identity**: Use `TabId` (newtype `u64`, monotonic counter) not UUID — simpler, sufficient for session scope
- **Tab bar location**: Top line (new layout chunk) vs integrated into existing status bar (bottom). Recommend: **top line** — status bar is already dense with agent status/timing info
- **Navigation keys**: `Ctrl+PageUp/Down` is the most conventional choice (matches browser/terminal tabs). Fallback: `/tabs` command palette
- **BTW thread persistence**: Keep current behavior — btw tabs create real persistent threads (users may want to revisit side conversations in thread history)
- **Shared vs tab-local boundary**: `config`, `tasks`, `auth`, `git_branch`, `display_path`, `optimistic_active_threads` = shared. `input`, `transcript`, `thread`, `agent_state`, `status_line`, `input_area`, `system_prompt`, `agent_opts` = tab-local.

# Testing
- Manual smoke demos per slice
- `just ci` must pass after each slice
- No new test files needed — existing integration tests cover the main chat flow
- If tab navigation introduces regressions in existing keybindings, add a targeted test

# Polish phases (after MVP)

## Phase 1: Tab UX polish
- Tab bar styling (colors, icons, rounded borders)
- Show tab title based on first message or thread title
- Tab reordering with keybindings
- ✅ Check-in demo: tabs look polished, have meaningful names

## Phase 2: Multi-tab productivity
- `/btw` with an initial prompt (e.g. `/btw what is X?`) — creates tab and immediately sends
- Tab-specific model/thinking overrides shown in tab bar
- Notification sound/flash for background tab completion
- ✅ Check-in demo: power-user workflow is smooth

# Later / Deferred
- **Tab persistence across restarts** — would need serializing tab state; revisit if users request it
- **Split-screen / side-by-side tabs** — significant render complexity; revisit after tab system is proven
- **Tab groups / workspaces** — organizational feature; revisit if tab count regularly exceeds 3-4
- **Drag-and-drop reorder** — mouse interaction complexity; revisit if users request it

# Oracle review notes
- **Shared/tab-local boundary is the #1 risk**: `status_line`, `agent_opts`, `system_prompt` must be tab-local. `config` effective model/thinking mutations need careful handling since overrides are currently applied into `tui.config`.
- **Use `split_active_mut()` helper**: avoid `app.active_tab_mut()` + `&mut app.shared` pattern — it borrows all of `app`. Instead split fields explicitly.
- **Unify event routing early**: replacing `BtwAgent`/`Agent` with `TabAgent { tab_id }` in Slice 2 prevents carrying two parallel event paths.
- **`input_area` cache**: currently set during render and read during mouse handling. Must be per-tab or active-only — decide in Slice 1.
- **Overlays that inspect transcript**: `ToolDetail` looks up `app.tui.transcript` during render. After tab restructure, overlays accessing transcript must use the active tab.
