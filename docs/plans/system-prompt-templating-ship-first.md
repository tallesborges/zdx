# Goals
- Add template-driven system prompt assembly for `zdx-core` context building.
- Keep existing behavior stable by default while enabling an opt-in template path.
- Preserve current context inputs (`system_prompt`, `AGENTS.md`, skills, subagents) in rendered output.
- Keep the external prompt reference as a raw link for implementation guidance.

# Non-goals
- Reworking provider-specific merge behavior.
- Replacing existing provider prompt template files.
- Adding features unrelated to prompt assembly in `core/context.rs`.

# Design principles
- User journey drives order
- Backward compatibility first
- Safe fallback over hard failure

# User journey
1. Enable template-based prompt assembly.
2. Run zdx and receive a rendered system prompt with conditional sections.
3. Verify current context behavior still works (`AGENTS.md`, skills, subagents).
4. Rely on deterministic fallback if template rendering fails.

# Foundations / Already shipped (✅)
List capabilities that already exist and should not be rebuilt.

## Effective system prompt resolution
- What exists: `system_prompt_file` takes precedence over inline `system_prompt`.
- ✅ Demo: Configure both and verify file content is selected.
- Gaps: No template rendering layer over structured context values.

## Project context ingestion (`AGENTS.md`)
- What exists: Hierarchical discovery, truncation warnings, unreadable-file warnings, merged context.
- ✅ Demo: Place multiple `AGENTS.md` files in hierarchy and verify merged prompt includes them in order.
- Gaps: Context is appended as static text, not conditionally rendered.

## Skills + subagents prompt injection
- What exists: Skills metadata and subagent model availability are appended to prompt output.
- ✅ Demo: Enable skills/subagents and verify both blocks appear.
- Gaps: No template-level placement/conditional control.

## Provider merge baseline
- What exists: `merge_system_prompt` always includes the base agentic prompt and appends caller prompt.
- ✅ Demo: Send request with and without caller prompt and verify merged shape.
- Gaps: No explicit template contract between context assembly and provider merge.

## External reference prompt
- What exists: Raw reference source is available.
- ✅ Demo: Open and review the reference template.
- Gaps: Not yet integrated into implementation docs.
- Reference: https://raw.githubusercontent.com/can1357/oh-my-pi/main/packages/coding-agent/src/prompts/system/system-prompt.md

# MVP slices (ship-shaped, demoable)
Define Slice 1..N in user-journey order.

## Slice 1: Template mode scaffold with compatibility fallback
- **Goal**: Add an opt-in template rendering path without changing default behavior.
- **Scope checklist**:
  - [ ] Introduce template mode toggle/config path for system prompt assembly.
  - [ ] Define minimal render context (base prompt + agents/skills/subagents + cwd/date).
  - [ ] On render failure, fall back to current concatenation with warning.
- **✅ Demo**: Enable template mode with a minimal template and verify zdx runs; disable and verify current output behavior remains.
- **Risks / failure modes**:
  - Silent behavior drift if fallback is not explicit.
  - Missing context fields causing empty sections.

## Slice 2: Conditional rendering for existing context blocks
- **Goal**: Make `AGENTS.md`, skills, and subagents sections conditionally renderable.
- **Scope checklist**:
  - [ ] Support conditionals for optional blocks.
  - [ ] Support list iteration for paths/skills/models where required.
  - [ ] Preserve current ordering and semantics under default template.
- **✅ Demo**: Run scenarios with none/partial/full context and verify expected section changes.
- **Risks / failure modes**:
  - Ordering drift compared to current behavior.
  - Prompt size growth from verbose templates.

## Slice 3: Default template parity pass
- **Goal**: Ship a default template that closely reproduces current structure for daily use.
- **Scope checklist**:
  - [ ] Add default template content aligned with current concatenation behavior.
  - [ ] Ensure existing prompt sources still flow through provider merge.
  - [ ] Validate no regression in request construction.
- **✅ Demo**: Compare effective prompt before/after under same inputs and verify parity on core sections.
- **Risks / failure modes**:
  - Unintended duplication with base provider prompt text.
  - Downstream assumptions about prompt shape breaking.

## Slice 4: Contract tests and hardening
- **Goal**: Lock behavior with focused regression tests.
- **Scope checklist**:
  - [ ] Add tests for `system_prompt_file` precedence.
  - [ ] Add tests for `AGENTS.md` hierarchy + warnings + truncation under template mode.
  - [ ] Add tests for render-failure fallback behavior.
- **✅ Demo**: Run targeted tests and verify prompt contract suite passes.
- **Risks / failure modes**:
  - Brittle snapshots when template text changes frequently.
  - Missed edge cases for missing variables.

# Contracts (guardrails)
List non-negotiable behaviors that must not regress (derived from Inputs and existing behavior).
- `system_prompt_file` precedence over inline prompt must remain unchanged.
- `AGENTS.md` loading order, warning behavior, and truncation behavior must remain unchanged.
- Skills and subagents metadata must continue to be available to the final prompt.
- Default behavior must remain stable when template mode is disabled.
- Provider merge flow must still include base agentic prompt plus caller/system additions.

# Key decisions (decide early)
List only decisions that would cause rework if postponed (derived from Inputs).
- Template engine choice and supported syntax subset.
- Render failure policy (warn-and-fallback vs hard fail).
- Canonical render context schema (field names and nesting).
- Template file location + precedence rules relative to existing config keys.

# Testing
- Manual smoke demos per slice
- Minimal regression tests only for contracts

# Polish phases (after MVP)
Group improvements into phases, each with a ✅ check-in demo.
Limited strictly to scope present in Inputs.

## Phase 1: Template ergonomics and docs
- Document supported fields/conditionals and migration path.
- Add concise troubleshooting guidance for render warnings/fallback.
- ✅ Check-in demo: A developer enables template mode and renders all current context blocks without reading source code.

## Phase 2: Prompt observability
- Add lightweight diagnostics for active template source/mode.
- Add diagnostics for missing variables in templates.
- ✅ Check-in demo: A render issue is diagnosable in one run.

# Later / Deferred
Explicit list of "not now" items + what would trigger revisiting them.
- Provider-specific prompt DSL divergence; revisit if adapters require incompatible prompt shapes.
- Broader prompt ecosystem parity beyond current context fields; revisit on demonstrated demand.
- Advanced helper/plugin systems for templates; revisit if minimal conditionals/lists are insufficient.