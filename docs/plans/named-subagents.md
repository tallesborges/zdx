# Goals
- `invoke_subagent(subagent: "task", prompt: "...")` and `invoke_subagent(subagent: "automation_assistant", prompt: "...")` cover the immediate normal-use and headless-use flows
- Named subagents configure: standalone prompt body + model + tools + thinking — stored as markdown files with YAML frontmatter
- Unified subagent consumers: subagent tool and automations (`subagent:` in frontmatter)
- Available subagents dynamically listed in tool description
- Users can create custom subagents at `~/.zdx/subagents/` (global) and `.zdx/subagents/` (project-level)
- Subagent prompt bodies are standalone prompts (no automatic inheritance from the default ZDX prompt/context pipeline)

# Non-goals
- Subagent inheritance / composability (subagents are flat)
- GUI/TUI for managing subagents
- Subagent validation CLI command (manual inspection is fine for alpha)
- Per-tool config within subagents (e.g., bash allowed paths)
- Runtime subagent switching in interactive TUI sessions

# Design principles
- User journey drives order
- Subagents are just files — no database, no migration
- Convention over configuration: built-in defaults work out of the box
- Project subagents override global subagents override built-in defaults (by name)
- Start with the minimum useful set of built-ins; add specialized subagents only after real usage proves the need
- Prefer a simple standalone-prompt model for subagents over implicit inheritance/composition

# User journey
1. User invokes delegated work without choosing a named subagent → works via the reserved `task` alias (default behavior mapping of today's normal mode)
2. User sets `subagent: automation_assistant` in an automation frontmatter → automation runs in a headless, non-interactive mode tuned to finish the task without depending on follow-up questions
3. User creates `~/.zdx/subagents/my-analyst.md` → it appears in available subagents
4. Later, user adds specialized subagents like `search`, `review`, `plan`, or `orchestrator` only when needed

# Foundations / Already shipped (✅)

## Subagent execution
- What exists: `invoke_subagent` tool with `prompt` + `subagent` params, `ExecSubagentOptions` with model/thinking/no_tools/no_system_prompt/timeout, spawns `zdx exec` child process
- ✅ Demo: `invoke_subagent(prompt: "echo hello")` works in interactive TUI
- Gaps: no per-tool filtering beyond explicit `--tools` allowlists in child exec args

## CLI exec flags
- What exists: `zdx exec --prompt X --model M --thinking T --tools "read,grep" --no-tools --no-system-prompt`
- ✅ Demo: `zdx exec -p "hello" --tools "read,grep" --no-system-prompt` runs with restricted tools
- Gaps: no `--system-prompt-override` flag to inject a custom system prompt string (only `--no-system-prompt` to disable or `--system-prompt` on root CLI to override config)

## ToolSet / ToolSelection
- What exists: `ToolSet::Default`, `ToolSet::OpenAICodex`, `ToolSelection::Explicit(Vec<String>)`, `--tools` CLI flag
- ✅ Demo: `--tools "read,grep,glob"` restricts to those tools
- Gaps: none for subagent needs — `--tools` already supports explicit list

# MVP slices (ship-shaped, demoable)

## Slice 1: Subagent file format + loader
- **Goal**: Define subagent struct, parse markdown+YAML frontmatter files, load with precedence (project > global > built-in)
- **Scope checklist**:
  - [ ] `SubagentDefinition` struct: `name`, `description`, `model` (Option), `thinking_level` (Option), `system_prompt` (String, from markdown body), `tools` (Option<Vec<String>>)
  - [ ] Subagent parser: read `.md` file → split YAML frontmatter + markdown body → deserialize into `SubagentDefinition`
  - [ ] `load_subagent(name, project_root)` → searches `.zdx/subagents/` then `~/.zdx/subagents/` then built-in defaults
  - [ ] `list_subagents(project_root)` → returns all available subagent names+descriptions (deduplicated by name, project wins)
  - [ ] Built-in subagents embedded via `include_str!`: `oracle`, `automation_assistant`
  - [ ] Built-in subagent markdown files in `crates/zdx-core/src/subagents/` (or similar)
  - [ ] Subagent markdown body is treated as a MiniJinja template, not just a literal string
  - [ ] Unit tests for parsing and precedence
- **✅ Demo**: Unit test loads built-in `oracle` and `automation_assistant` subagents, verifies model/tools/system_prompt fields
- **Risks / failure modes**:
  - YAML frontmatter parsing edge cases → mitigate: reuse same parsing as skills/automations if they already parse frontmatter

## Slice 2: Wire subagents into subagent execution
- **Goal**: `invoke_subagent(subagent: "task", prompt: "...")` or `invoke_subagent(subagent: "automation_assistant", prompt: "...")` resolves the requested runtime behavior and passes config to child `zdx exec`
- **Scope checklist**:
  - [ ] Add `subagent` param to `invoke_subagent` tool schema (optional string, replaces `model`)
  - [ ] Remove `model` param from tool schema (named subagent owns model selection)
  - [ ] In `execute()`: resolve subagent → build `ExecSubagentOptions` from subagent fields
  - [ ] Treat the subagent markdown body as the complete child system prompt
  - [ ] Pass custom system prompt to child: add `--system-prompt <TEXT>` support to `ExecSubagentOptions` / `build_exec_args` (reuse existing root `--system-prompt` CLI flag)
  - [ ] Pass `--tools` list from subagent to child exec args
  - [ ] Keep `--no-system-prompt` as an advanced CLI escape hatch, not part of the subagent schema
  - [ ] Default to the `task` runtime alias when no subagent is specified
  - [ ] Dynamic tool description: inject available subagent names+descriptions into `Invoke_Subagent` tool description
  - [ ] Update system prompt `<subagents>` block to reference named subagents instead of `<available_models>`
  - [ ] Update tests
- **✅ Demo**: In TUI, agent uses `invoke_subagent(subagent: "automation_assistant", prompt: "summarize yesterday's thread activity into a structured report")` → child runs with automation prompt/constraints and completes without asking follow-up questions
- **Risks / failure modes**:
  - System prompt text might be very long when passed as CLI arg → mitigate: use `--system-prompt-file` or temp file if needed; for MVP, CLI arg is fine (OS arg limit is 256KB+)

## Slice 3: Automation subagent integration
- **Goal**: Automations use `subagent:` to configure their exec runs
- **Scope checklist**:
  - [ ] Add optional `subagent` field to automation YAML frontmatter schema
  - [ ] When automation has `subagent: X`, resolve subagent and apply model/tools/system_prompt to the automation's exec run
  - [ ] Update automation validation to check subagent exists
- **✅ Demo**: Create automation with `subagent: automation_assistant` in frontmatter → `just automations run <name>` uses automation prompt/constraints and finishes in headless mode
- **Risks / failure modes**:
  - Automation already has `model:` in frontmatter — need to decide precedence (subagent wins, explicit model overrides subagent) → decide in key decisions

# Contracts (guardrails)
- `invoke_subagent` without `subagent` must still work (defaults to the base/default prompt behavior)
- Built-in subagents must always be available even with no user files
- Subagent `tools` list is an allowlist — child exec gets exactly those tools
- Named subagent prompt bodies are standalone system prompts and do not append to the default system prompt composition
- Project-level subagents override global subagents override built-in (by name match)
- Existing `zdx exec` CLI flags continue to work independently of named subagents
- `automation_assistant` subagent must be written for non-interactive execution: complete the task with reasonable assumptions instead of depending on user clarification
- Subagent prompt rendering must support the same template capabilities as the current system prompt rendering path

# Key decisions (decide early)
- **Subagent `model` format**: use the existing `provider:model` format (e.g., `gemini:gemini-2.5-flash-lite`)
- **Automation `model:` vs `subagent:`**: when both present, explicit `model:` in frontmatter overrides subagent's model (subagent provides defaults, frontmatter overrides specific fields)
- **System prompt passthrough mechanism**: reuse existing root-level `--system-prompt` CLI flag (already wired to override config system prompt)
- **Frontmatter parser**: reuse whatever skills/automations already use for YAML frontmatter parsing (avoid new dependency if possible)
- **Subagent context model**: named subagents are standalone prompts; omitted `subagent` keeps default composition

# Testing
- Manual smoke demos per slice
- Unit tests for subagent parsing + precedence in Slice 1
- Unit tests for `build_exec_args` with subagent-derived options in Slice 2
- Unit tests proving subagent prompt templates support the same basic behavior as the current system prompt templates (for example `if`, `for`, and existing prompt-template vars)
- Integration test: `zdx exec` with `--system-prompt` + `--tools` produces expected behavior

# Polish phases (after MVP)

## Phase 1: Subagent UX improvements
- `zdx subagents list` CLI command showing all available subagents with descriptions
- `zdx subagents show <name>` to inspect a subagent
- Warn on unknown subagent name instead of silent failure
- ✅ Check-in demo: `zdx subagents list` shows built-in + user subagents with descriptions

## Phase 2: Subagent refinements
- Add specialized built-ins only after they prove useful in practice: `search`, `review`, `plan`, `orchestrator`, `bot`
- Subagent `extends:` field for inheriting from another subagent
- `tools_add` / `tools_remove` fields (modify base tool set instead of full override)
- Subagent-specific timeout configuration
- ✅ Check-in demo: custom subagent with `extends: oracle` inherits oracle defaults and overrides model

# Later / Deferred
- **Subagent templates/generators** — create a subagent from an interactive wizard. Trigger: users frequently create subagents with similar patterns.
- **Subagent marketplace/sharing** — import subagents from URLs/repos. Trigger: community demand.
- **Per-subagent environment variables** — inject env vars into child process. Trigger: subagents need API keys or runtime config.
- **TUI subagent picker** — select a subagent from an overlay menu. Trigger: TUI becomes primary subagent consumer.
- **Specialized built-ins (`search`, `review`, `plan`, `orchestrator`, `bot`)** — add once the `task` alias + `automation_assistant`/`oracle` pattern is dogfooded enough to show clear repeated patterns.
