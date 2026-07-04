# Goals
- Ship a `/prompt-builder` feature that turns a short natural-language intent into a ready-to-use prompt.
- Insert the generated prompt into the composer instead of auto-sending it.
- Let the user save a generated prompt as a custom command.
- Let the user save a generated prompt as a skill draft.
- Let the user generate prompt variants such as `short`, `strict`, `oracle-heavy`, and `explorer-heavy`.

# Non-goals
- Full workflow execution or stateful looping in MVP.
- Auto-running the generated prompt immediately after creation.
- Replacing custom commands or skills — this feature feeds them.
- Hardcoded prompt families in Rust for every use case.
- Multi-step prompt refinement sessions with persistent history.

# Design principles
- User journey drives order
- Generate first, execute later.
- Insert into composer, do not auto-send.
- Prompt-builder is a launcher for reusable prompting, not a workflow engine.
- Save paths should reuse existing/custom command and skill conventions instead of inventing parallel storage.
- Variants are optional accelerators, not required for the base flow.

# User journey
1. User invokes `/prompt-builder`.
2. ZDX asks for a short intent such as “I just finished the implementation and need a multi-pass Oracle review loop.”
3. ZDX generates a polished prompt and inserts it into the composer.
4. User can edit it, send it, or choose a follow-up action.
5. Optional follow-up actions:
   - save as command
   - save as skill draft
   - generate variants
6. Later, the user can run the saved command or finish turning the draft into a real skill.

# Foundations / Already shipped (✅)

## Command palette + slash command surfaces
- What exists: slash-command palette in TUI, command execution path, and existing built-in commands.
- ✅ Demo: open command palette and run built-ins like `/new`.
- Gaps: no prompt-builder command yet.

## Custom commands foundation
- What exists: custom Markdown commands already load from `$ZDX_HOME/commands/*.md` and `<cwd>/.zdx/commands/*.md`, and they appear in the command palette.
- ✅ Demo: custom commands show in the palette.
- Gaps: command selection still needs composer insertion wiring in later slices of `custom-commands.md`, but the storage/discovery model already exists.

## Skills system
- What exists: file-based skills with YAML frontmatter + markdown body, bundled fallbacks, and discovery from user/project locations.
- ✅ Demo: `ship-first-plan` and `deep-interview` are bundled and load correctly.
- Gaps: no “skill draft” creation flow yet.

## Composer/input insertion path
- What exists: the TUI already supports mutating the input/composer text from UI effects.
- ✅ Demo: built-in command flows already mutate UI state in controlled ways.
- Gaps: no dedicated “insert generated prompt” effect yet.

# Architecture placement
- Prompt generation logic should live in `zdx-engine` or a shared prompt helper layer so multiple surfaces can reuse it.
- The first activation surface should be TUI; Telegram support can come after the core interaction proves useful.
- Saving as command/skill draft should reuse the existing file formats and directories rather than inventing a new artifact system.

# Core concept
- `/prompt-builder` is not a skill and not a workflow.
- It is a user-facing prompt construction tool:
  - input: user intent
  - output: generated prompt text
- The generated prompt can then flow into three destinations:
  1. insert into composer
  2. save as command
  3. save as skill draft

# Output modes

## Base output
- A single polished prompt that matches the user’s intent.
- Default behavior: insert into composer.

## Variants
- Optional generated variants from the same base intent:
  - `short`
  - `strict`
  - `oracle-heavy`
  - `explorer-heavy`
- Variants are generated on demand after the base prompt exists.

## Save targets
- **Save as command**
  - writes a Markdown command file into `$ZDX_HOME/commands/` or `.zdx/commands/`
  - minimal frontmatter: `description`
  - body = generated prompt template
- **Save as skill draft**
  - writes a draft skill file into a draft-friendly location (see Key decisions)
  - includes starter frontmatter (`name`, `description`) and the generated body
  - draft is not automatically bundled or activated as a polished skill

# MVP slices (ship-shaped, demoable)

## Slice 1: TUI prompt-builder command + composer insertion
- **Status**: ✅ Implemented and reviewed (Oracle pass).
- **Goal**: User can invoke `/prompt-builder`, describe what they want, and get a generated prompt inserted into the composer.
- **Scope checklist**:
  - [x] Add `/prompt-builder` built-in command to the command palette
  - [x] Add a lightweight input flow for the builder request (modal, inline prompt, or dedicated overlay)
  - [x] Generate a prompt from the user’s intent using a dedicated builder prompt/template
  - [x] Add a UI effect that inserts the generated prompt into the composer
  - [x] Do not auto-send the generated prompt
- **What changed**:
  - New asset `crates/zdx-assets/prompts/prompt_builder_prompt.md` + `PROMPT_BUILDER_PROMPT_TEMPLATE` (re-exported through `zdx-engine::prompts`).
  - New `PromptBuilderState` (`Idle`/`Pending`/`Generating`) on `InputState`, plus `InputMutation::SetPromptBuilderState`.
  - New `TaskKind::PromptBuilder` + `TaskMeta::PromptBuilder { intent }`, `UiEffect::StartPromptBuilder`, `UiEvent::PromptBuilderResult`.
  - Runtime task in `crates/zdx-tui/src/runtime/prompt_builder.rs` reusing `run_exec_subagent_with_cancel` (no_tools, no_system_prompt, minimal thinking, 120s timeout). Active model is used.
  - Command palette registers `prompt-builder` (aliases: `builder`, `prompt`, category `prompt`) and dispatches to `execute_prompt_builder`. Mutual-exclusion guards: `execute_handoff` rejects when prompt-builder is active (and now runs the prompt-builder check before the active-thread check); `execute_prompt_builder` rejects when handoff is active or when prompt-builder is already active. Custom-command palette selection is also blocked while either flow owns the composer.
  - Input reducer: pending submission → emits `StartPromptBuilder`; Generating → blocks Enter and shows hint to press Esc; Esc cancels the running task via `UiEffect::CancelTask { kind: PromptBuilder }`; "queue while running" path also blocks while builder is active. Modal handoff/prompt-builder submissions now run **before** slash/bash command parsing in `submit_input`, so an intent that happens to look like `/fast` or `$echo hi` does not bleed through.
  - On `PromptBuilderResult::Ok`, the generated prompt is dropped into the composer via `set_text` and state returns to `Idle` (no auto-send). On `Err`, the user's intent is restored and state returns to `Pending` so they can retry.
  - Render: extracted shared `render_status_input` helper used by both handoff and prompt-builder modes; prompt-builder shows yellow "describe your intent" border in Pending and cyan "generating prompt..." border in Generating.
  - `crates/zdx-tui/AGENTS.md` updated to list `runtime/prompt_builder.rs`.
- **Oracle review**:
  - **High** (fixed): handoff palette dispatch did not block when prompt-builder was active → added symmetric guards (and reordered the prompt-builder check ahead of the active-thread check so it always wins).
  - **High** (fixed): prompt-builder pending submission was parsed as slash/bash before being treated as builder intent → moved modal submissions before `handle_slash_commands`/`handle_bash_commands`.
  - **Medium** (fixed): `/prompt-builder` re-entry while pending or generating could clobber the in-flight session → added self-guard.
  - **Medium** (fixed): refactored `render_status_input` was claiming placeholder highlighting was preserved; in reality every span is forced white (matching the original handoff behaviour) → comment corrected.
  - **Low** (fixed): TUI `AGENTS.md` was missing the new runtime module entry.
- **New regression tests**:
  - `runtime::prompt_builder::tests::substitutes_intent_placeholder_verbatim`, `template_keeps_role_framing`
  - `overlays::command_palette::tests::test_palette_prompt_builder_command_arms_pending_state`
  - `overlays::command_palette::tests::test_palette_prompt_builder_blocked_during_handoff`
  - `overlays::command_palette::tests::test_palette_handoff_blocked_during_prompt_builder`
  - `overlays::command_palette::tests::test_palette_prompt_builder_blocked_when_already_active`
  - `features::input::update::tests::prompt_builder_pending_submission_emits_start_effect`
  - `features::input::update::tests::prompt_builder_pending_intent_starting_with_slash_is_not_treated_as_slash_command`
  - `features::input::update::tests::prompt_builder_empty_pending_submission_keeps_state`
  - `features::input::update::tests::prompt_builder_result_inserts_prompt_into_composer_and_returns_to_idle`
  - `features::input::update::tests::prompt_builder_failure_restores_intent_and_returns_to_pending`
- **Verification**:
  - `just ci` — clean (lint + all crate tests) after Oracle fixes; one flaky CLI MCP probe test (`test_mcp_auth_reports_dcr_failure_with_guidance`) intermittently fails standalone but passes when re-run; unrelated to this slice.
- **✅ Demo**: User runs `/prompt-builder`, types “make me a bug investigation loop with Oracle,” presses Enter, and sees the generated prompt appear in the composer.
- **Risks / failure modes**:
  - Generated prompt is too vague or too long
  - UX may feel clunky if the input flow is too modal or hides too much context

## Slice 2: Save generated prompt as command
- **Status**: ✅ Implemented and reviewed (Oracle pass).
- **Goal**: User can take a generated prompt and save it directly as a reusable custom command.
- **Scope checklist**:
  - [x] Add “Save as command” action after prompt generation
  - [x] Ask for command name + optional description
  - [x] Write Markdown command file to `$ZDX_HOME/commands/` by default
  - [x] Reuse the custom command file format already implemented/planned
  - [x] Show the saved path in the transcript/status area
- **Deviations / decisions**:
  - **UX shape**: chose a generic `/save-as-command` palette command (aliases `save-cmd`, `save-command`) that operates on the **current composer text** rather than a hard-coded continuation of the prompt-builder modal. This:
    - keeps slice 2 decoupled from slice 1 (any prompt in the composer can be saved, including manually authored prompts)
    - satisfies the Phase 4 "promote-to-command from chat history" path with the same mechanism (covered by the new test exercising `set_text` → `/save-as-command`)
    - mirrors slice 1's modal pattern (composer is reused for the name input)
  - **Description**: skipped the optional description prompt for MVP. Files are written body-only (no frontmatter); the parser already treats missing frontmatter as "no description". Users can edit the file later to add one. The plan's "minimal frontmatter: description" line is interpreted as "if any frontmatter exists, only use `description`", which the loader already enforces.
  - **Save location**: hardcoded to `$ZDX_HOME/commands/<name>.md` per the plan ("Default to `$ZDX_HOME/commands/`, with project-local save as a later option"). Project-local save is deferred to Phase 2.
- **What changed**:
  - New engine helpers in `crates/zdx-engine/src/custom_commands.rs`:
    - `is_valid_command_name(name)` — allow ASCII alphanumerics, `-`, `_`; must start with a letter/digit.
    - `user_commands_dir()` — `<ZDX_HOME>/commands`.
    - `WriteCustomCommandError` enum (`InvalidName`, `EmptyContent`, `AlreadyExists`, `Io`).
    - `write_custom_command_in_dir(dir, name, content) -> Result<PathBuf, _>` — explicit-dir entry point used by tests; refuses to overwrite existing files.
    - `write_user_custom_command(name, content) -> Result<PathBuf, _>` — thin wrapper using `user_commands_dir()`.
  - New `SaveAsCommandState` (`Idle` / `AwaitingName { content }`) on `InputState`, `InputMutation::SetSaveAsCommandState`.
  - New `UiEffect::SaveCustomCommand { name, content }` and `UiEvent::CustomCommandSaved { name, result }`.
  - `/save-as-command` registered in the palette (category `prompt`); `execute_save_as_command` captures the composer text, refuses if any other modal flow owns the composer or if the composer is empty, transitions to `AwaitingName`, and clears the composer for the name input.
  - Mutual-exclusion guards extended: handoff/prompt-builder palette dispatch and custom-command palette selection now also reject when save-as-command is active.
  - Input reducer:
    - `handle_save_as_command_submission` runs **before** slash/bash parsing (same ordering fix Slice 1 introduced for handoff/prompt-builder), so a typed name like `/fast` cannot be re-routed.
    - Empty / invalid name → advisory + state preserved so the user can retype.
    - Valid name → `UiEffect::SaveCustomCommand { name, content }`; modal stays in `AwaitingName` until the runtime returns a result.
    - Esc cancels the modal.
    - Queue-while-running guard rejects new sends while save-as-command is active.
  - Render: shared `render_status_input` extended with a `render_save_as_command_input` thin wrapper. The original `render_input_with_cursor` exceeded `clippy::too_many_lines`; extracted a `try_render_modal_input` helper to dispatch all three modal renderers.
  - Runtime handler for `UiEffect::SaveCustomCommand`:
    - Calls `write_user_custom_command` synchronously (file I/O is fast; no need to spawn a task).
    - Dispatches `UiEvent::CustomCommandSaved` with the path or error.
    - On success, reloads custom commands so the new entry shows up in the palette without restarting.
  - Result handler `handle_custom_command_saved`:
    - Success → state to `Idle`, transcript shows `Saved /<name> to <path>`.
    - Failure → state stays in `AwaitingName` (captured content preserved), transcript shows `save-as-command failed: <error>` so the user can pick a different name.
- **New regression tests**:
  - `zdx-engine::custom_commands::tests`: `test_is_valid_command_name_accepts_typical_names`, `test_is_valid_command_name_rejects_invalid_inputs`, `test_write_custom_command_in_dir_writes_markdown_file`, `test_write_custom_command_in_dir_preserves_trailing_newline`, `test_write_custom_command_in_dir_rejects_invalid_inputs`, `test_write_custom_command_in_dir_refuses_to_overwrite_existing_file`, `test_write_custom_command_in_dir_creates_missing_directory`.
  - `zdx-tui::overlays::command_palette::tests`: `test_palette_save_as_command_arms_awaiting_name_with_captured_content`, `test_palette_save_as_command_refuses_empty_composer`, `test_palette_save_as_command_blocked_during_handoff`.
  - `zdx-tui::features::input::update::tests`: `save_as_command_valid_name_emits_save_effect`, `save_as_command_invalid_name_keeps_modal_and_advises`, `save_as_command_pending_intent_starting_with_slash_is_not_treated_as_slash_command`, `save_as_command_success_returns_to_idle_with_path_message`, `save_as_command_failure_keeps_awaiting_name_for_retry`.
- **Verification**:
  - `cargo check -p zdx-tui` — clean.
  - `just clippy` — clean (`-D warnings`); had to extract `try_render_modal_input` to keep `render_input_with_cursor` under the 100-line limit.
  - `cargo test -p zdx-engine` — 335 passing.
  - `cargo test -p zdx-tui --lib` — 291 passing.
  - `just ci` — clean.
- **Oracle review**:
  - **High** (fixed): `/save-as-command` could write a file whose name shadowed a built-in/alias and report success even though the loader would silently skip it. Added a pre-write reserved-name check in `handle_save_as_command_submission` against `builtin_command_identifiers()` (case-insensitive); modal stays in `AwaitingName` so the user can retype.
  - **Low** (fixed): replaced the `path.exists()` + `fs::write` pair with `OpenOptions::create_new(true)` so the "refuses to overwrite" contract is atomic and TOCTOU-free.
  - **Low** (deferred): no max length on command names. Easy hardening; not required for slice 3.
  - **Test gap** (fixed): added save-as-command vs handoff/prompt-builder/custom-command-selection mutual-exclusion tests; added built-in primary-name and alias rejection tests (`save_as_command_rejects_builtin_command_name`, `save_as_command_rejects_builtin_alias_case_insensitively`).
- **✅ Demo**: Generate a prompt (or write any prompt manually), open the palette and pick `/save-as-command`, type `review-loop`, press Enter, and see `$ZDX_HOME/commands/review-loop.md` created plus a "Saved /review-loop to ..." line in the transcript.
- **Risks / failure modes**:
  - Name conflicts with built-ins or existing custom commands
  - Saving a giant prompt may create an ugly command file unless formatting is normalized

## Slice 3: Save generated prompt as skill draft
- **Status**: ✅ Implemented and reviewed (Oracle pass).
- **Goal**: User can convert a generated prompt into a draft skill file for later refinement.
- **Scope checklist**:
  - [x] Add “Save as skill draft” action after prompt generation
  - [x] Ask for draft skill name + description
  - [x] Write a skill draft file with starter frontmatter + body
  - [x] Keep draft output separate from polished bundled skills
  - [x] Clearly label drafts as user-owned unfinished artifacts
- **Deviations / decisions**:
  - **UX shape**: same modal-composer pattern as `/save-as-command` — added `/save-as-skill-draft` (aliases `save-skill`, `skill-draft`) that operates on the **current composer text** and asks for a draft name. This means any prompt in the composer can be promoted to a draft skill, not just generated ones.
  - **Description**: deliberately not asked. The starter SKILL.md frontmatter ships with a placeholder description (`"Draft skill saved from prompt-builder. Edit this description to summarize when to use the skill."`) so the file is a valid skill out of the box, and the user is nudged to edit before promoting it. This keeps the modal flow single-step, matching slice 2's UX.
  - **Save location**: `<ZDX_HOME>/skill-drafts/<name>/SKILL.md`. Verified that the skill loader only scans `<ZDX_HOME>/skills/` (and other configured roots), NOT `<ZDX_HOME>/skill-drafts/`, so drafts are NEVER auto-discovered as live skills.
  - **Reuse**: skill-name validation matches the loader's internal rules (lowercase ASCII letters, digits, `-`; no leading/trailing/double dashes; ≤ 64 chars). Public helper `is_valid_skill_name` mirrors the private `validate_name` rules so the TUI and engine agree without exposing the loader's internal validator.
- **What changed**:
  - **Engine** (`crates/zdx-engine/src/skills.rs`):
    - `SKILL_DRAFTS_DIR_NAME` constant (`"skill-drafts"`).
    - `user_skill_drafts_dir() -> PathBuf` returning `<ZDX_HOME>/skill-drafts`.
    - `is_valid_skill_name(name) -> bool`.
    - `WriteSkillDraftError` enum (`InvalidName`, `EmptyContent`, `AlreadyExists`, `Io`).
    - `write_skill_draft_in_dir(root, name, body) -> Result<PathBuf, _>` — explicit-dir entry point used by tests; `OpenOptions::create_new(true)` makes the existence check atomic; writes `<root>/<name>/SKILL.md` with starter frontmatter (`name`, `description`).
    - `write_user_skill_draft(name, body) -> Result<PathBuf, _>` — thin wrapper.
  - **TUI**:
    - `SaveAsSkillDraftState` (`Idle` / `AwaitingName { content }`) on `InputState`, plus `InputMutation::SetSaveAsSkillDraftState`.
    - `UiEffect::SaveSkillDraft { name, content }` and `UiEvent::SkillDraftSaved { name, result }`.
    - `/save-as-skill-draft` registered (category `prompt`); `execute_save_as_skill_draft` shares logic with `execute_save_as_command` via a new `capture_save_target_content` / `modal_busy_or_empty_advisory` helper pair.
    - Mutual-exclusion guards extended throughout: handoff/prompt-builder/save-as-command palette dispatch and custom-command palette selection now also reject when save-as-skill-draft is active.
    - Input reducer: `handle_save_as_skill_draft_submission` runs in the modal-priority block (before slash/bash). Validates the name via `zdx_engine::skills::is_valid_skill_name`. Esc cancels the modal. Queue-while-running guard rejects new sends while the draft modal is active.
    - Render: `render_save_as_skill_draft_input` reuses the shared `render_status_input` helper with a yellow `save-as-skill-draft (enter skill name, Esc to cancel)` border.
    - Runtime: synchronous handler for `UiEffect::SaveSkillDraft` calling `write_user_skill_draft`, dispatches `UiEvent::SkillDraftSaved`. No `custom_commands` reload because drafts are never live.
    - Result handler `handle_skill_draft_saved` mirrors `handle_custom_command_saved`: success → `Idle` + `Saved skill draft \`<name>\` to <path>` transcript message; failure → modal stays in `AwaitingName` for retry.
- **New regression tests**:
  - `zdx-engine::skills::tests`: `test_is_valid_skill_name_accepts_typical_names`, `test_is_valid_skill_name_rejects_invalid_inputs`, `test_write_skill_draft_in_dir_writes_file_with_starter_frontmatter`, `test_write_skill_draft_in_dir_round_trips_through_loader` (round-trips a draft through the skill loader to ensure the starter frontmatter we emit is parseable), `test_write_skill_draft_in_dir_rejects_invalid_inputs`, `test_write_skill_draft_in_dir_refuses_to_overwrite_existing_dir`.
  - `zdx-tui::overlays::command_palette::tests`: `test_palette_save_as_skill_draft_arms_awaiting_name_with_captured_content`, `test_palette_save_as_skill_draft_refuses_empty_composer`, `test_palette_save_as_skill_draft_blocked_during_save_as_command`.
  - `zdx-tui::features::input::update::tests`: `save_as_skill_draft_valid_name_emits_save_effect`, `save_as_skill_draft_invalid_name_keeps_modal_and_advises`, `save_as_skill_draft_pending_name_starting_with_slash_is_not_treated_as_slash_command`, `save_as_skill_draft_success_returns_to_idle_with_path_message`, `save_as_skill_draft_failure_keeps_awaiting_name_for_retry`.
- **Verification**:
  - `cargo test -p zdx-engine skills` — 30 passing (5 new draft tests added).
  - `just clippy` — clean (`-D warnings`).
  - `just ci` — clean across the workspace; engine 341 passing, TUI 304 passing.
- **Oracle review**: no blockers. Two low-priority items addressed:
  - **Low** (fixed): test coverage gap on the four-flow guard matrix → added `test_palette_save_as_skill_draft_blocked_during_handoff`, `test_palette_save_as_skill_draft_blocked_during_prompt_builder`, `test_palette_handoff_blocked_during_save_as_skill_draft`, `test_palette_prompt_builder_blocked_during_save_as_skill_draft`, `test_palette_custom_command_selection_blocked_during_save_as_skill_draft`.
  - **Nit** (fixed): `capture_save_target_content` had an unused `_action_label` parameter; removed.
  - **Low** (deferred): partial-write directory cleanup. Marked optional by Oracle and not a normal-path bug; left as-is to avoid speculative complexity.
- **✅ Demo**: Generate a planning prompt, type `/save-as-skill-draft`, name it `plan-with-oracle`, press Enter, and see `<ZDX_HOME>/skill-drafts/plan-with-oracle/SKILL.md` created with starter frontmatter and the prompt body. The skill is NOT picked up by the live `/skills` overlay.
- **Risks / failure modes**:
  - Users may mistake a draft for a polished production skill
  - Draft location may conflict with discovered live skills if not separated clearly

## Slice 4: Prompt variants
- **Status**: ✅ Implemented and reviewed (Oracle pass).
- **Goal**: User can generate alternate prompt variants from the same intent.
- **Scope checklist**:
  - [x] Add “Generate variants” action after base prompt generation
  - [x] Support at least 4 variants: `short`, `strict`, `oracle-heavy`, `explorer-heavy`
  - [x] Let user pick one variant to insert into composer or save
  - [x] Keep the base prompt available alongside variants
- **Deviations / decisions**:
  - **UX shape**: same generic palette-command pattern as slices 2 and 3 — `/variants` (aliases `variant`, `prompt-variants`) operates on the **current composer text** (the base prompt). One LLM call returns all four labeled variants; a small picker overlay lets the user pick one.
  - **Variant set**: hardcoded curated set of 4 (`short`, `strict`, `oracle-heavy`, `explorer-heavy`). Plan explicitly says "do not allow arbitrary variant names in MVP".
  - **Picker UX**: standalone `VariantsPicker` overlay with arrow-key navigation, number-key shortcut (1–4), Enter to insert, Esc to cancel. The picker shows a description per variant + a preview of the currently selected variant.
  - **Base preservation**: when generation succeeds, the runtime restores the captured base prompt to the composer (so Esc out of the picker leaves the user's base prompt intact and editable). The picker holds the variants in its own state.
  - **Output format**: the variants subagent emits a labeled-section format (`SHORT:` / `STRICT:` / `ORACLE-HEAVY:` / `EXPLORER-HEAVY:`) rather than JSON; case-insensitive parser with tolerance for preamble/trailing whitespace. Rejected outputs surface as a transcript advisory; the picker is not opened.
- **What changed**:
  - **Engine**:
    - New asset `crates/zdx-assets/prompts/prompt_variants_prompt.md` + `PROMPT_VARIANTS_PROMPT_TEMPLATE` (re-exported through `zdx-engine::prompts`).
    - New `PromptVariantKind` enum + `PromptVariants` struct + `parse_prompt_variants` parser in `crates/zdx-engine/src/prompts.rs`. 5 unit tests cover happy path, preamble tolerance, missing-section error, empty-section error, and case-insensitive headers.
  - **TUI**:
    - New `TaskKind::PromptVariants` + `TaskMeta::PromptVariants { base }`.
    - New `VariantsState` (`Idle` / `Generating { base }`) on `InputState` + `InputMutation::SetVariantsState`.
    - New `UiEffect::StartPromptVariants` and `UiEvent::PromptVariantsResult { base, result: Result<PromptVariants, String> }`.
    - New `crates/zdx-tui/src/overlays/variants_picker.rs` (`VariantsPickerState`) with arrow keys, number-key shortcut (1–4), Enter to insert, Esc to cancel. 4 unit tests.
    - New `OverlayRequest::VariantsPicker { variants }` + `Overlay::VariantsPicker(state)` wired through `open_overlay_request` in `update.rs`.
    - New `crates/zdx-tui/src/runtime/prompt_variants.rs` runtime handler reusing `run_exec_subagent_with_cancel` (same options as prompt-builder, 180s timeout). Calls `parse_prompt_variants` on the raw output before dispatching the result event.
    - `/variants` registered in the palette (category `prompt`); `execute_variants` shares the `capture_save_target_content` / `modal_busy_or_empty_advisory` helpers with the save flows.
    - Mutual-exclusion guards extended to all four-flow combinations: handoff, prompt-builder, save-as-command, save-as-skill-draft, and `/variants` palette dispatch + custom-command palette selection now all reject when any of the others is active.
    - Input reducer:
      - Generation phase blocks Enter with a "Press Esc to cancel" advisory; queue-while-running guard rejects new sends.
      - Esc in the Generating state cancels via `UiEffect::CancelTask { kind: PromptVariants }` and restores the captured base to the composer.
      - `handle_prompt_variants_result` restores base on success or failure; success returns an `OverlayRequest::VariantsPicker` for the main reducer to open; failure surfaces a transcript advisory.
      - Refactored `handle_control_keys` Esc branch into `handle_esc_voice` + `handle_esc_modals` to stay under `clippy::too_many_lines`. Refactored `submit_input` generation-block branches into `block_submit_during_generation`.
    - Render: `render_variants_input` (cyan border, "generating short / strict / oracle-heavy / explorer-heavy...") via shared `render_status_input`.
- **New regression tests**:
  - `zdx-engine::prompts::tests`: `parse_prompt_variants_extracts_all_four_sections`, `parse_prompt_variants_tolerates_preamble_and_trailing_whitespace`, `parse_prompt_variants_rejects_missing_variant`, `parse_prompt_variants_rejects_empty_variant_body`, `parse_prompt_variants_is_case_insensitive_on_headers`.
  - `zdx-tui::runtime::prompt_variants::tests`: `substitutes_base_placeholder_verbatim`, `template_keeps_role_framing`.
  - `zdx-tui::overlays::variants_picker::tests`: `enter_inserts_selected_variant_and_closes`, `number_key_shortcut_selects_corresponding_variant`, `esc_closes_without_mutations`, `down_arrow_clamps_to_last_variant`.
  - `zdx-tui::overlays::command_palette::tests`: `test_palette_variants_arms_generating_state_and_emits_start_effect`, `test_palette_variants_refuses_empty_composer`, `test_palette_variants_blocked_during_other_modals`, `test_palette_handoff_blocked_during_variants`, `test_palette_custom_command_selection_blocked_during_save_as_skill_draft`.
  - `zdx-tui::features::input::update::tests`: `variants_success_restores_base_and_returns_overlay_request`, `variants_failure_restores_base_and_emits_advisory`.
- **Verification**:
  - `cargo test -p zdx-engine prompts` — 7 parser tests passing (5 happy-path + duplicate-header + label-like-body).
  - `just clippy` — clean (`-D warnings`).
  - `just ci` — clean across the workspace; engine 348 passing, TUI 318 passing.
- **Oracle review**:
  - **High** (fixed): user could type into the composer while a generation phase was active; the result/Esc handler then overwrote the typed text with the captured base. Added `InputState::is_modal_generation_active()` and a guard at the top of `handle_main_key` that drops everything except control keys (Esc/Ctrl+C/voice hotkey) and Enter (which still shows the existing "press Esc to cancel" advisory). Applied across handoff, prompt-builder, and variants generation phases. Added `typing_is_dropped_silently_during_modal_generation` and `esc_during_modal_generation_still_cancels_after_typing_guard` regression tests.
  - **Medium** (fixed): `parse_prompt_variants` silently overwrote on duplicate headers — a label-like line inside an earlier body could corrupt the picker. Now fails closed with `duplicate \`<label>\` section`. Added `parse_prompt_variants_rejects_duplicate_header` and `parse_prompt_variants_rejects_label_like_line_in_body` tests.
  - **Low** (fixed): `crates/zdx-engine/AGENTS.md` description of `prompts.rs` updated to mention the new variant types and parser.
- **✅ Demo**: Generate a base prompt (or write one manually), open the palette and pick `/variants`. Wait while the cyan "generating..." border shows. When the picker opens, navigate with ↑/↓, optionally press 1–4 to jump directly to a variant, press Enter on `strict` to replace the composer with the strict variant. Esc out of the picker leaves the composer with the base prompt intact.
- **Risks / failure modes**:
  - Variants may be too similar to each other
  - Variant generation may overfit and produce needlessly complex prompts

# Contracts (guardrails)
- `/prompt-builder` never auto-sends the generated prompt.
- Generated prompts remain editable by the user before sending or saving.
- Saving as command must reuse the custom-commands format and location conventions.
- Saving as skill draft must not silently create a live polished bundled skill.
- Variant generation must be explicit and user-triggered.
- Built-in commands still win over saved command names.

# Key decisions (decide early)
- **Activation surface**: start with TUI only; Telegram later if the interaction proves useful.
- **Builder implementation**: use a dedicated internal builder prompt/template rather than hardcoded string assembly.
- **Default destination**: generated prompt goes to the composer, not transcript-only output.
- **Command save location**: default to `$ZDX_HOME/commands/`, with project-local save as a later option.
- **Skill draft location**: use a draft-specific area (for example `$ZDX_HOME/skill-drafts/`) so drafts do not immediately behave like polished live skills.
- **Variant scope**: ship a small curated set first; do not allow arbitrary variant names in MVP.

# Testing
- Manual smoke demos per slice
- Minimal regression tests for:
  - prompt-builder command appears in palette
  - generated prompt inserts into composer
  - save-as-command writes expected Markdown file
  - save-as-skill-draft writes expected draft file
  - variant selection replaces/inserts the chosen prompt correctly

# Polish phases (after MVP)

## Phase 1: Telegram + chat surfaces
- Add prompt-builder interaction to Telegram and other non-TUI surfaces.
- Keep the generated prompt as an editable artifact before send.
- ✅ Check-in demo: build a prompt from Telegram and receive it back ready to reuse.

## Phase 2: Project-local save targets
- Let the user choose between user-global and project-local command save destinations.
- Add project-local skill-draft save path if useful.
- ✅ Check-in demo: save a prompt as `.zdx/commands/review-loop.md` in the current project.

## Phase 3: Richer variants + prompt provenance
- Add more variants (for example `concise`, `teaching`, `debug-heavy`).
- Record which builder intent and variant produced a saved command/draft.
- ✅ Check-in demo: inspect a saved command and see builder provenance metadata.

## Phase 4: Promote-to-command / promote-to-skill from chat history
- Let the user take a manually written prompt from the transcript/composer and save it directly as a command or draft skill.
- ✅ Check-in demo: select a successful prompt from history and turn it into a reusable command without re-running builder.

# Later / Deferred
- Auto-detecting when the user “probably wants prompt-builder” without explicit invocation.
- Full workflow generation from prompt-builder output.
- Sharing commands/skill drafts across repos automatically.
- Auto-curation or ranking of the user’s saved prompts.