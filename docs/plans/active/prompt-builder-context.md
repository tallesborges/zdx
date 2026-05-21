# Goals
- Replace the hardcoded subagent vocabulary in the `/prompt-builder` template with a runtime-built manifest of the user's actual installed subagents, skills, and custom commands.
- Let the user opt the current thread transcript into the `/prompt-builder` generation call via a keybind in the Pending modal.
- Keep the default `/prompt-builder` output suitable for reusable prompts (save-as-command, save-as-skill-draft, variants).

# Non-goals
- Auto-detecting when thread context "should" be included.
- Filtering the manifest by intent (relevance ranking) in MVP.
- Adding manifest/transcript support to surfaces other than the TUI `/prompt-builder` flow (e.g. `/variants`, handoff, Telegram).
- Provenance metadata on saved commands/drafts indicating which context was used.
- Changing the existing `{{INTENT}}` placeholder name or template shape beyond adding new optional sections.

# Design principles
- User journey drives order.
- Always-on context first (smaller blast radius, no UX surface), opt-in context second (new keybind, new state).
- Default behavior must not silently change for saved/reusable outputs.
- Reuse existing loaders and helpers; do not invent parallel listing/transcript code.
- Keep the manifest compact (name + one-line description per entry); template stays readable.

# User journey
1. User opens `/prompt-builder` and types a short intent.
2. The generator already knows what subagents, skills, and custom commands are available in this project and references them by their real names instead of a hardcoded set.
3. If the user wants the prompt grounded in the current chat, they hit a keybind in the Pending modal to toggle "include thread context"; the modal shows the toggle state.
4. User presses Enter; the generator runs with manifest (always) + transcript (only when toggled on) and inserts the prompt into the composer.
5. User edits/sends, or routes through `/save-as-command`, `/save-as-skill-draft`, or `/variants` as today.

# Foundations / Already shipped (✅)

## `/prompt-builder` core flow
- What exists: Modal `PromptBuilderState` (Idle / Pending / Generating / Ready) in `crates/zdx-tui/src/features/input/state.rs:149-166`; `UiEffect::StartPromptBuilder { intent: String }` at `crates/zdx-tui/src/effects.rs:157`; spawn site at `crates/zdx-tui/src/runtime/mod.rs:1016-1024`; runtime task `prompt_builder_generation` in `crates/zdx-tui/src/runtime/prompt_builder.rs`; template asset `crates/zdx-assets/prompts/prompt_builder_prompt.md` (one `{{INTENT}}` placeholder) re-exported as `PROMPT_BUILDER_PROMPT_TEMPLATE` via `crates/zdx-assets/src/lib.rs:18` and `crates/zdx-engine/src/prompts.rs:14`.
- ✅ Demo: Run `/prompt-builder`, type an intent, see a generated prompt in the composer.
- Gaps: generator sees only `{{INTENT}}`; subagent names are hardcoded inside the template.

## Subagent listing
- What exists: `subagents::discover(root)` at `crates/zdx-engine/src/subagents.rs:156` returns `Vec<SubagentDefinition>` (project + built-in merged); built-ins at `subagents::built_in_definitions()` (`crates/zdx-engine/src/subagents.rs:411`). Each `SubagentDefinition` (`crates/zdx-engine/src/subagents.rs:117`) carries `name` + `description`.
- ✅ Demo: Existing system-prompt assembly already calls this path.
- Gaps: none for read; need a thin "manifest entry" projection.

## Skill listing
- What exists: `skills::load_skills(&LoadSkillsOptions)` at `crates/zdx-engine/src/skills.rs:247` returns the merged built-in / user-global / project-local skill set; each `Skill` (`crates/zdx-engine/src/skills.rs:173`) has `name`, `description`, `file_path`. The active skill list is already carried through `ThreadUiEvent::Created` (`crates/zdx-tui/src/events.rs:94`).
- ✅ Demo: Skills already feed the system prompt.
- Gaps: confirm where the active skill list is stashed in `TuiState` after thread creation so the runtime task can reuse it instead of re-loading.

## Custom command listing
- What exists: `custom_commands::load_custom_commands(cwd, builtin_names)` at `crates/zdx-engine/src/custom_commands.rs:94`; `command_dirs_for_cwd` walks ancestor `.zdx/commands/` (`crates/zdx-engine/src/custom_commands.rs:156`); the palette already enumerates the loaded set via `app.custom_commands` in `crates/zdx-tui/src/state.rs:136`.
- ✅ Demo: Custom commands show up in the palette today.
- Gaps: none for read; reuse the already-loaded `app.custom_commands` snapshot.

## Thread transcript loading
- What exists: `zdx_engine::core::thread_persistence::load_thread_events(thread_id)` and `format_transcript(&events)` (`crates/zdx-engine/src/core/thread_persistence.rs`, `format_transcript` near `:2176`). Already wired into `crates/zdx-tui/src/runtime/handoff.rs::load_thread_content`.
- ✅ Demo: `/handoff` already produces a transcript-grounded prompt.
- Gaps: `handoff.rs::load_thread_content` is private. Either lift it to a shared helper or call `thread_persistence` directly from the prompt-builder task.

## Modal keybind handling
- What exists: `handle_overlays` in `crates/zdx-tui/src/features/input/update.rs:627` routes hotkeys; `Ctrl+B` already opens `/prompt-builder`. `Ctrl+T` is taken by the thinking picker (`crates/zdx-tui/src/features/input/update.rs:616`). A boolean toggle pattern exists in `build_fast_mode_toggle_actions` at `crates/zdx-tui/src/features/input/update.rs:48`.
- ✅ Demo: existing toggles in fast-mode / thinking-picker flow.
- Gaps: no per-modal local toggle pattern today; need a small new field on `PromptBuilderState::Pending` and a keybind active only while that modal owns the composer.

# MVP slices (ship-shaped, demoable)

## Slice 1: Always-on project-context manifest
- **Goal**: The generator sees a compact manifest of the user's actual subagents, skills, and custom commands and references them by real names instead of the hardcoded `Oracle / Explorer / Thread Searcher / Task` block.
- **Scope checklist**:
  - [ ] Add a new placeholder `{{PROJECT_CONTEXT}}` to `crates/zdx-assets/prompts/prompt_builder_prompt.md`; replace the "ZDX subagent vocabulary" section's hardcoded names with template guidance that says "use the entries listed in `{{PROJECT_CONTEXT}}` when they fit; do not invent ones not listed."
  - [ ] Add an engine-side helper that builds a `String` manifest from `subagents::discover` + currently-loaded skills + currently-loaded custom commands. Format: three labeled sections (`Subagents:` / `Skills:` / `Custom commands:`), each a flat `- name — one-line description` list. Truncate descriptions to a single line. Place it in a new module or extend `crates/zdx-engine/src/prompts.rs`.
  - [ ] Extend `UiEffect::StartPromptBuilder` (`crates/zdx-tui/src/effects.rs:157`) to carry the pre-built manifest string, OR have the runtime build it from `TuiState` at the `StartPromptBuilder` handler in `crates/zdx-tui/src/runtime/mod.rs:1016` using the already-loaded skills/custom-commands snapshots + a fresh `subagents::discover(root)` call. Prefer the runtime-side build so the input reducer stays pure.
  - [ ] Extend `prompt_builder_generation` in `crates/zdx-tui/src/runtime/prompt_builder.rs` to accept the manifest and substitute it into the template alongside `{{INTENT}}`.
  - [ ] Update `build_prompt_builder_prompt` and existing unit tests in `crates/zdx-tui/src/runtime/prompt_builder.rs` accordingly.
- **✅ Demo**: With a project that has at least one user-installed skill or custom command, run `/prompt-builder` with an intent like "build me a planning loop"; the generated prompt names actual user-installed artifacts (e.g. references `ship-first-plan` or a user's custom command) instead of the static `Oracle/Explorer/Thread Searcher/Task` block alone.
- **Risks / failure modes**:
  - Manifest grows large in repos with many skills/commands; cap or summarize if it bloats the prompt.
  - Generator over-eagerly inserts every listed skill into the output; mitigate via template wording ("reference only when clearly relevant").
  - Manifest churn between calls makes regression tests on the generated prompt brittle; assert on placeholder substitution and section presence, not full output.

## Slice 2: Opt-in thread transcript context
- **Goal**: A keybind in the `/prompt-builder` Pending modal flips an "include thread context" flag; when on, the runtime loads the current thread transcript and feeds it to the generator.
- **Scope checklist**:
  - [ ] Add `include_thread: bool` to `PromptBuilderState::Pending` (and carry through `Generating { intent, include_thread }`) in `crates/zdx-tui/src/features/input/state.rs:149-166`; default `false`.
  - [ ] Add a keybind in `handle_overlays` / the Pending-state key path in `crates/zdx-tui/src/features/input/update.rs:627` that toggles `include_thread` while the modal is in Pending. Pick a key not already bound (candidate: `Ctrl+R` for "reference thread"; final choice is a Key Decision below).
  - [ ] Update the Pending-state render (`render_prompt_builder_input` in `crates/zdx-tui/src/features/input/render.rs:474`) to surface the toggle in the border/footer text, e.g. `prompt-builder (describe your intent · thread: off · Ctrl+R to toggle · Esc to cancel)`.
  - [ ] Extend `UiEffect::StartPromptBuilder` and `prompt_builder_generation` to carry `include_thread`. When true, look up the active thread id from `TuiState` (`self.state.tui.thread.thread_handle.as_ref().map(|h| h.id.clone())` per the spawn site), load events via `thread_persistence::load_thread_events`, format with `format_transcript`, and pass it into a new `{{THREAD_CONTEXT}}` placeholder. When false or when no thread is active, substitute an empty string and let the template treat the absence as "general request — do not assume conversation context."
  - [ ] Add `{{THREAD_CONTEXT}}` to `crates/zdx-assets/prompts/prompt_builder_prompt.md` with one short instruction block: "If `{{THREAD_CONTEXT}}` is non-empty, ground the prompt in it; otherwise treat the intent as a general template request and do not invent conversation details."
  - [ ] If thread loading fails or there is no active thread while `include_thread` is true, surface a transcript advisory and restore the modal to Pending (mirrors the existing failure-restores-intent pattern in `crates/zdx-tui/src/features/input/update.rs` around `PromptBuilderResult::Err`).
- **✅ Demo**: Open `/prompt-builder` mid-conversation, press the toggle key, see the Pending modal border switch to `thread: on`, type an intent like "turn this debugging session into a written runbook", press Enter, and see a prompt that visibly references concrete details from the thread. Repeat with the toggle off and see a generic version of the same prompt.
- **Risks / failure modes**:
  - Saved/reusable outputs may overfit if a user leaves the toggle on while running `/save-as-command` afterwards; toggle is per-builder-invocation and resets to off on the next Idle → Pending transition.
  - Long transcripts blow the generator's token budget; reuse handoff's existing pattern (it already runs on full transcripts with a 120s timeout) and rely on the same `run_exec_subagent_with_cancel` cancellation path.
  - Existing `prompt_builder_generation` is `no_tools: true`; transcript loading happens before the subagent call (same shape as `handoff_generation`), so this constraint is preserved.

# Contracts (guardrails)
- `/prompt-builder` still never auto-sends the generated prompt.
- Default behavior (no manifest, no thread) of any single keystroke must not change: an unmodified `/prompt-builder` invocation without the new keybind must behave like today aside from the manifest substitution in Slice 1.
- The thread-context toggle is per-invocation; it must reset to `false` whenever the modal returns to `Idle`.
- `{{INTENT}}` placeholder name and semantics must not change.
- Manifest substitution must not duplicate or contradict the hardcoded subagent guidance the template still keeps for fallback wording.
- Built-in subagent names (`Oracle`, `Explorer`, `Thread Searcher`, `Task`) still appear in the manifest because `discover` merges built-ins; the template should not need to hardcode them anymore.

# Key decisions (decide early)
- **Manifest assembly site**: build in the runtime handler at `crates/zdx-tui/src/runtime/mod.rs:1016` using already-loaded `TuiState` snapshots, or pre-assemble in the input reducer and ship it inside `UiEffect::StartPromptBuilder`. Recommended: runtime-side (keeps input reducer pure, avoids re-loading skills/subagents).
- **Manifest format**: plain text, three labeled sections, one `- name — description` line per entry. Cap per-entry description to the first line of the source description field; no truncation count for MVP.
- **Toggle keybind**: `Ctrl+R` (free; mnemonic "reference thread") vs `Ctrl+J` vs a non-Ctrl modifier. `Ctrl+T` is taken by the thinking picker. Decide before Slice 2 starts.
- **Effect shape**: extend `UiEffect::StartPromptBuilder` with `include_thread: bool` (and optionally `project_context: String` if we choose pre-assembly) vs add a second effect. Recommended: extend the existing effect — it is a single feature and adding a sibling effect would duplicate spawn plumbing.
- **Transcript helper sharing**: lift `handoff::load_thread_content` into a shared helper in `zdx-engine` (or call `thread_persistence` directly from the builder task). Recommended: call `thread_persistence` directly to avoid coupling builder ↔ handoff.

# Testing
- Manual smoke demos per slice (above).
- Minimal regression tests (no new heavy suites):
  - `prompt_builder_prompt.md` substitutes `{{INTENT}}`, `{{PROJECT_CONTEXT}}`, and `{{THREAD_CONTEXT}}` verbatim and leaves no placeholder behind.
  - Manifest builder emits the three section headers and at least the built-in subagent names when given a discovery result with built-ins.
  - Modal toggle: starting at `include_thread=false`, the toggle keybind flips it to `true` and back; transitioning out of `Pending` resets to `false`.
  - With `include_thread=true` but no active thread, the runtime surfaces an advisory and restores `Pending` (mirrors existing failure tests around `PromptBuilderResult::Err`).
  - `UiEffect::StartPromptBuilder` carries the new fields (`include_thread`, optionally `project_context`) and the existing spawn site forwards them.

# Polish phases (after MVP)

## Phase 1: Manifest relevance / size control
- Trim the manifest to top-N entries when total length exceeds a threshold; bias toward project-local entries over bundled ones.
- ✅ Check-in demo: in a project with 30+ custom commands the builder still receives a short, relevant manifest and the prompt remains tight.

## Phase 2: Toggle persistence + variants integration
- Plumb `{{PROJECT_CONTEXT}}` (always) and optional `{{THREAD_CONTEXT}}` into the variants generator (`/variants`) so generated variants also benefit from real-artifact awareness.
- Optionally remember the last toggle choice within the same TUI session.
- ✅ Check-in demo: run `/variants` after a transcript-grounded `/prompt-builder` invocation and see variants that respect the same context shape.

## Phase 3: Telegram + non-TUI surfaces
- Extend the same manifest + opt-in transcript shape to other surfaces once the TUI flow proves valuable.
- ✅ Check-in demo: build a prompt from Telegram with project context auto-included.

# Later / Deferred
- Auto-deciding when to include thread context (heuristics or LLM-classified intent).
- Persisting context-inclusion choices across sessions or per-project defaults.
- Provenance metadata on saved commands/drafts recording manifest + thread inclusion at generation time.
- Sharing or syncing manifests across machines/repos.
