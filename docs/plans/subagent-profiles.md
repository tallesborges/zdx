# Goals
- `invoke_subagent(profile: "general_assistant", prompt: "...")` and `invoke_subagent(profile: "automation_assistant", prompt: "...")` cover the immediate normal-use and headless-use flows
- Named profiles configure: system_prompt + model + tools + thinking — stored as markdown files with YAML frontmatter
- Unified profile consumers: subagent tool, automations (`profile:` in frontmatter), telegram bot
- Available profiles dynamically listed in tool description
- Users can create custom profiles at `~/.zdx/profiles/` (global) and `.zdx/profiles/` (project-level)

# Non-goals
- Profile inheritance / composability (profiles are flat)
- GUI/TUI for managing profiles
- Profile validation CLI command (manual inspection is fine for alpha)
- Per-tool config within profiles (e.g., bash allowed paths)
- Runtime profile switching in interactive TUI sessions

# Design principles
- User journey drives order
- Profiles are just files — no database, no migration
- Convention over configuration: built-in defaults work out of the box
- Project profiles override global profiles override built-in defaults (by name)
- Start with the minimum useful set of built-ins; add specialized profiles only after real usage proves the need

# User journey
1. User invokes subagent without changes → works with built-in `general_assistant` profile (default behavior mapping of today's normal mode)
2. User sets `profile: automation_assistant` in an automation frontmatter → automation runs in a headless, non-interactive mode tuned to finish the task without depending on follow-up questions
3. Telegram bot uses `general_assistant` profile → no AGENTS.md/coding context pollution, but still keeps general assistant behavior
4. User creates `~/.zdx/profiles/my-analyst.md` → it appears in available profiles
5. Later, user adds specialized profiles like `search`, `review`, `plan`, or `orchestrator` only when needed

# Foundations / Already shipped (✅)

## Subagent execution
- What exists: `invoke_subagent` tool with `prompt` + `model` params, `ExecSubagentOptions` with model/thinking/no_tools/no_system_prompt/timeout, spawns `zdx exec` child process
- ✅ Demo: `invoke_subagent(prompt: "echo hello")` works in interactive TUI
- Gaps: no profile param, no custom system_prompt passthrough, no per-tool filtering (only `--no-tools` or `--tools` comma list)

## CLI exec flags
- What exists: `zdx exec --prompt X --model M --thinking T --tools "read,grep" --no-tools --no-system-prompt`
- ✅ Demo: `zdx exec -p "hello" --tools "read,grep" --no-system-prompt` runs with restricted tools
- Gaps: no `--system-prompt-override` flag to inject a custom system prompt string (only `--no-system-prompt` to disable or `--system-prompt` on root CLI to override config)

## ToolSet / ToolSelection
- What exists: `ToolSet::Default`, `ToolSet::OpenAICodex`, `ToolSelection::Explicit(Vec<String>)`, `--tools` CLI flag
- ✅ Demo: `--tools "read,grep,glob"` restricts to those tools
- Gaps: none for profile needs — `--tools` already supports explicit list

# MVP slices (ship-shaped, demoable)

## Slice 1: Profile file format + loader
- **Goal**: Define profile struct, parse markdown+YAML frontmatter files, load with precedence (project > global > built-in)
- **Scope checklist**:
  - [ ] `Profile` struct: `name`, `description`, `model` (Option), `thinking_level` (Option), `system_prompt` (String, from markdown body), `tools` (Option<Vec<String>>)
  - [ ] Profile parser: read `.md` file → split YAML frontmatter + markdown body → deserialize into `Profile`
  - [ ] `load_profile(name, project_root)` → searches `.zdx/profiles/` then `~/.zdx/profiles/` then built-in defaults
  - [ ] `list_profiles(project_root)` → returns all available profile names+descriptions (deduplicated by name, project wins)
  - [ ] Built-in profiles embedded via `include_str!`: `general_assistant`, `automation_assistant`
  - [ ] Built-in profile markdown files in `crates/zdx-core/src/profiles/` (or similar)
  - [ ] Unit tests for parsing and precedence
- **✅ Demo**: Unit test loads built-in `general_assistant` and `automation_assistant` profiles, verifies model/tools/system_prompt fields
- **Risks / failure modes**:
  - YAML frontmatter parsing edge cases → mitigate: reuse same parsing as skills/automations if they already parse frontmatter

## Slice 2: Wire profiles into subagent execution
- **Goal**: `invoke_subagent(profile: "general_assistant", prompt: "...")` or `invoke_subagent(profile: "automation_assistant", prompt: "...")` resolves profile and passes config to child `zdx exec`
- **Scope checklist**:
  - [ ] Add `profile` param to `invoke_subagent` tool schema (optional string, replaces `model`)
  - [ ] Remove `model` param from tool schema (profile owns model selection)
  - [ ] In `execute()`: resolve profile → build `ExecSubagentOptions` from profile fields
  - [ ] Pass custom system prompt to child: add `--system-prompt <TEXT>` support to `ExecSubagentOptions` / `build_exec_args` (reuse existing root `--system-prompt` CLI flag)
  - [ ] Pass `--tools` list from profile to child exec args
  - [ ] Keep `--no-system-prompt` as an advanced CLI escape hatch, not part of the profile schema
  - [ ] Default to `general_assistant` profile when no profile specified
  - [ ] Dynamic tool description: inject available profile names+descriptions into `Invoke_Subagent` tool description
  - [ ] Update system prompt `<subagents>` block to reference profiles instead of `<available_models>`
  - [ ] Update tests
- **✅ Demo**: In TUI, agent uses `invoke_subagent(profile: "automation_assistant", prompt: "summarize yesterday's thread activity into a structured report")` → child runs with automation prompt/constraints and completes without asking follow-up questions
- **Risks / failure modes**:
  - System prompt text might be very long when passed as CLI arg → mitigate: use `--system-prompt-file` or temp file if needed; for MVP, CLI arg is fine (OS arg limit is 256KB+)
  - Removing `model` param is breaking for existing usage → mitigate: keep `model` as deprecated fallback in Slice 2, remove in polish

## Slice 3: Automation + bot profile integration
- **Goal**: Automations and telegram bot use `profile:` to configure their exec runs
- **Scope checklist**:
  - [ ] Add optional `profile` field to automation YAML frontmatter schema
  - [ ] When automation has `profile: X`, resolve profile and apply model/tools/system_prompt to the automation's exec run
  - [ ] Telegram bot: configure default profile (e.g., `general_assistant`) in `[telegram]` config section
  - [ ] Bot uses profile's system_prompt/tools/model instead of default coding context
  - [ ] Update automation validation to check profile exists
- **✅ Demo**: Create automation with `profile: automation_assistant` in frontmatter → `just automations run <name>` uses automation prompt/constraints and finishes in headless mode
- **Risks / failure modes**:
  - Automation already has `model:` in frontmatter — need to decide precedence (profile wins, explicit model overrides profile) → decide in key decisions

# Contracts (guardrails)
- `invoke_subagent` without `profile` must still work (defaults to `general_assistant`)
- Built-in profiles must always be available even with no user files
- Profile `tools` list is an allowlist — child exec gets exactly those tools
- Profile system_prompt replaces (not appends to) the default system prompt composition
- Project-level profiles override global profiles override built-in (by name match)
- Existing `zdx exec` CLI flags continue to work independently of profiles
- `automation_assistant` profile must be written for non-interactive execution: complete the task with reasonable assumptions instead of depending on user clarification

# Key decisions (decide early)
- **Profile `model` format**: use the existing `provider:model` format (e.g., `gemini:gemini-2.5-flash-lite`)
- **`model` param on tool schema**: keep as deprecated optional override in Slice 2 (profile takes precedence); remove in polish
- **Automation `model:` vs `profile:`**: when both present, explicit `model:` in frontmatter overrides profile's model (profile provides defaults, frontmatter overrides specific fields)
- **System prompt passthrough mechanism**: reuse existing root-level `--system-prompt` CLI flag (already wired to override config system prompt)
- **Frontmatter parser**: reuse whatever skills/automations already use for YAML frontmatter parsing (avoid new dependency if possible)
- **Profile context model**: profile prompt replaces default composition; `--no-system-prompt` remains only as an advanced CLI escape hatch outside the profile schema

# Testing
- Manual smoke demos per slice
- Unit tests for profile parsing + precedence in Slice 1
- Unit tests for `build_exec_args` with profile-derived options in Slice 2
- Integration test: `zdx exec` with `--system-prompt` + `--tools` produces expected behavior

# Polish phases (after MVP)

## Phase 1: Profile UX improvements
- `zdx profiles list` CLI command showing all available profiles with descriptions
- `zdx profiles show <name>` to inspect a profile
- Warn on unknown profile name instead of silent failure
- ✅ Check-in demo: `zdx profiles list` shows built-in + user profiles with descriptions

## Phase 2: Profile refinements
- Add specialized built-ins only after they prove useful in practice: `search`, `review`, `plan`, `orchestrator`, `bot`
- Profile `extends:` field for inheriting from another profile
- `tools_add` / `tools_remove` fields (modify base tool set instead of full override)
- Profile-specific timeout configuration
- ✅ Check-in demo: custom profile with `extends: general_assistant` inherits general_assistant defaults and overrides model

# Later / Deferred
- **Profile templates/generators** — create profile from interactive wizard. Trigger: users frequently create profiles with similar patterns.
- **Profile marketplace/sharing** — import profiles from URLs/repos. Trigger: community demand.
- **Per-profile environment variables** — inject env vars into child process. Trigger: profiles need API keys or runtime config.
- **TUI profile picker** — select profile from overlay menu. Trigger: TUI becomes primary profile consumer.
- **Specialized built-ins (`search`, `review`, `plan`, `orchestrator`, `bot`)** — add once the two-profile MVP (`general_assistant` + `automation_assistant`) is dogfooded enough to show clear repeated patterns.
