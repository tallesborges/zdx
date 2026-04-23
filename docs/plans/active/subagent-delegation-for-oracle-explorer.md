# Goals
- Oracle can delegate broad codebase exploration to Explorer
- Built-in subagents inherit the right ambient project/runtime context without manual prompt boilerplate
- Delegation is restricted: read-only subagents cannot reach `task` or write-capable agents
- No infinite recursion: the delegation graph is a one-way DAG

# Non-goals
- Generic depth counter (the DAG prevents loops for built-ins)
- User-defined subagent delegation graphs (future)
- Allowing `thread-searcher` to delegate
- Allowing `oracle` ↔ `explorer` bidirectional delegation
- Explorer → Thread Searcher delegation (deferred — `thread-searcher` has `bash`, which breaks the read-only boundary)

# Design principles
- User journey drives order
- Engine-enforced allowlist, not just prompt guidance
- Read-only boundary must be preserved — no indirect access to `task`/`bash`/`edit`/`write`

# User journey
1. User asks zdx a complex question requiring both deep analysis and broad exploration
2. Main agent delegates to oracle for diagnosis
3. Oracle autonomously delegates to explorer for evidence gathering
4. Results flow back up: explorer → oracle → main agent

# Foundations / Already shipped (✅)

## Subagent system
- What exists: `invoke_subagent` tool, `SubagentDefinition` struct, discovery/resolution, isolated `zdx exec` child processes
- ✅ Demo: `just run` → invoke any subagent via `invoke_subagent`
- Gaps: no `subagents` restriction field, no filtering of available subagents per definition, no built-in context inheritance policy for named subagents

## Built-in subagents (explorer, oracle, thread-searcher)
- What exists: definitions in `crates/zdx-assets/subagents/*.md`, each with explicit tool lists
- ✅ Demo: main agent can invoke all three
- Gaps: none have `invoke_subagent` in their tools

## Child process spawning
- What exists: parent builds `ExecSubagentOptions` with explicit `tools_override`, `model`, `system_prompt`, `thinking_level` → spawns `zdx exec` with `--tools`, `--effective-system-prompt-file`, `-m` flags. `definition_with_subagents` enriches the Invoke_Subagent schema with all discovered subagents.
- Key files: `crates/zdx-engine/src/tools/subagent.rs:160-188`, `crates/zdx-engine/src/core/agent.rs:1348-1356`, `crates/zdx-engine/src/core/subagent.rs:141-188`
- Gaps: child doesn't know its own identity, no per-subagent filtering of available subagents, parent must extract and pass everything explicitly, subagent prompts have no runtime environment context (CWD, OS, git, env vars) — the model doesn't know its runtime context even though the OS process has it

## Prompt template vars (already available)
- What exists: `render_standalone_prompt_template` already provides Tera vars `{{ cwd }}`, `{{ os }}`, `{{ arch }}`, `{{ git_repo_root }}`, `{{ git_branch }}`, `{{ date }}` etc. to subagent templates (`crates/zdx-engine/src/core/context.rs:371-443`)
- Gaps: current subagent templates don't use these vars, so useful ambient context is silently omitted. Requiring every subagent template to manually opt in is easy to forget and duplicates boilerplate.

# MVP slices (ship-shaped, demoable)

## Slice 1: `subagents` allowlist — authorization plumbing
- **Goal**: Engine supports a `subagents` field in frontmatter that restricts which subagents a child can invoke. Enforcement at both schema and runtime level.
- **Scope checklist**:
  - [ ] Add `subagents: Option<Vec<String>>` to `SubagentFrontmatter` and `SubagentDefinition` (`crates/zdx-engine/src/subagents.rs:117-148`)
  - [ ] Add allowed-subagents state to `AgentOptions` (`crates/zdx-engine/src/core/agent.rs:48-59`) and `ToolContext` (`crates/zdx-engine/src/tools/mod.rs:34-79`)
  - [ ] Parent extracts `definition.subagents` and passes it to the child via `ExecSubagentOptions` → new CLI flag (e.g. `--allowed-subagents explorer`)
  - [ ] In tool definition building (`crates/zdx-engine/src/core/agent.rs:1348-1356`), filter `list_summaries()` to only allowed names. Also handle `task`: `supported_subagent_names()` always pushes `task` (`crates/zdx-engine/src/tools/subagent.rs:295-303`) — must suppress it when not in the allowlist
  - [ ] Make the allowlist drive tool description/help text too, not just the enum. Restricted children must not see guidance that tells them to use `task` when `task` is disallowed
  - [ ] In `execute()` (`crates/zdx-engine/src/tools/subagent.rs:57`), enforce: reject invocations of subagents not in the allowlist (defense in depth beyond schema filtering)
  - [ ] When `subagents` is set but does not include `task`, block the `subagent: None` fallback that would resolve to default/task behavior
  - [ ] Absent `subagents` field = no restriction (backward-compatible, preserves `task` behavior)
  - [ ] Handle `subagents: []` — note: `normalize_named_items()` (`crates/zdx-engine/src/subagents.rs:610-626`) rejects empty lists, needs a dedicated path for this case
  - [ ] Validation: if a subagent has `invoke_subagent` in its tools but no `subagents` field, log a warning (fail-open is intentional but should be visible)
  - [ ] Add unit tests: allowed named subagent, blocked named subagent, blocked omitted `subagent` (task fallback), blocked explicit `task`, `subagents: []`
- **✅ Demo**: Define a test subagent with `subagents: [explorer]` and `tools: [invoke_subagent, read]`. Verify it can invoke explorer but not oracle/task/thread-searcher.
- **Risks / failure modes**:
  - `deny_unknown_fields` on `SubagentFrontmatter` means the `subagents` field must be added to the struct before any definition uses it
  - `normalize_named_items()` rejects empty lists — need to handle `subagents: []` as a valid "no delegation" case

## Slice 2: `--as-subagent` identity + defaults
- **Goal**: Child processes know their identity via `--as-subagent <name>`, derive defaults from their definition (prompt, tools, model, thinking level). Simplifies parent→child spawning for named subagents.
- **Scope checklist**:
  - [ ] Add `--as-subagent <name>` CLI flag to `zdx exec` (`crates/zdx-cli/`)
  - [ ] When set, child looks up its own definition by name and derives: system prompt, tools, model, thinking level, allowed subagents
  - [ ] `--as-subagent` conflicts with `--tools`, `--no-tools`, `--effective-system-prompt-file`, `--no-system-prompt` (mutually exclusive — definition is the source of truth)
  - [ ] `-m` and `-t` remain as optional overrides (parent may override model/thinking from definition defaults)
  - [ ] `--allowed-subagents` from Slice 1 is also derived from the definition when `--as-subagent` is used (no need to pass separately)
  - [ ] Update `ExecSubagentOptions` and `build_exec_options()` to prefer `--as-subagent` for named subagents, falling back to explicit flags for `task`
  - [ ] Add unit tests for identity resolution and flag conflict detection
- **✅ Demo**: `zdx exec --as-subagent oracle --prompt-file prompt.md` produces the same behavior as the current explicit `--effective-system-prompt-file` + `--tools` + `-m` + `-t` path.
- **Risks / failure modes**:
  - Named subagents without explicit `model` currently fall back to parent/config model (`crates/zdx-engine/src/tools/subagent.rs:198-214`). With `--as-subagent`, must preserve this fallback via `-m` override from parent
  - Same for `thinking_level` — definition may not have one, parent passes it via `-t`

## Slice 3: Per-subagent context inheritance policy
- **Goal**: Named subagents can inherit structured ambient context from the engine without every template manually embedding Tera vars.
- **Scope checklist**:
  - [ ] Add a `context` policy to subagent frontmatter, e.g.
    ```yaml
    context:
      environment: true
      project_context: true
    ```
  - [ ] Parse/store the policy in `SubagentFrontmatter` / `SubagentDefinition` (`crates/zdx-engine/src/subagents.rs`)
  - [ ] Apply context inheritance in `subagents::render_prompt()` only, not in `render_standalone_prompt_template()` — keep the generic renderer unchanged
  - [ ] Append standardized blocks after the rendered role prompt based on policy:
    - `<environment>`: `cwd`, `os`, `arch`, `git_repo_root`, `git_branch`, `date`
    - `<project_context>`: inherited AGENTS/CLAUDE inline context plus scoped paths/references only, not raw file contents
  - [ ] Keep existing Tera vars available as an escape hatch for custom placement/formatting, but do not require templates to use them
  - [ ] Built-in defaults:
    - `explorer`: environment + project_context
    - `oracle`: environment + project_context
    - `thread-searcher`: environment only
  - [ ] Default policy for other/user/project subagents: all false (opt-in, avoids silent behavior changes)
  - [ ] Do NOT inject `ZDX_THREAD_ID` or `ZDX_ARTIFACT_DIR` — children run with `--no-thread` (`crates/zdx-engine/src/core/subagent.rs:52-55`) so these are reset/meaningless for the child
  - [ ] Cap appended scoped path references aggressively for subagent prompts to avoid token blowups during parallel delegation
- **✅ Demo**: Invoke `oracle` and `explorer` and verify their rendered prompts include environment + project-context blocks with capped scoped references. Invoke `thread-searcher` and verify it includes only environment.
- **Risks / failure modes**:
  - Token growth if project context is large, especially for fan-out delegation
  - Global auto-append would be a silent behavior change for user/project subagents — avoid by making policy explicit and opt-in
  - Duplication if a template also manually renders the same context vars; built-ins should stop doing that once policy exists

## Slice 4: Oracle gets `invoke_subagent` → Explorer
- **Goal**: Oracle can delegate broad exploration to Explorer during deep analysis.
- **Scope checklist**:
  - [ ] Add `invoke_subagent` to oracle's tools list in `crates/zdx-assets/subagents/oracle.md`
  - [ ] Add `subagents: [explorer]` to oracle's frontmatter
  - [ ] Update oracle's prompt body with delegation guidance: "Use `invoke_subagent(subagent: 'explorer')` when you need multi-step broad codebase discovery that would take several search rounds. Use your own tools for targeted lookups."
- **✅ Demo**: Ask zdx a question requiring both deep analysis and broad exploration. Oracle should invoke explorer for evidence gathering, then synthesize the results.
- **Risks / failure modes**:
  - Oracle's model (gpt-5.4) may over-delegate simple lookups to explorer instead of using its own tools
  - Latency increases when oracle delegates (nested process spawning)

# Contracts (guardrails)
- Read-only subagents (explorer, oracle) MUST NOT gain indirect write access through delegation
- `subagents` allowlist MUST be enforced at the engine level, not just prompt guidance
- Omitting the `subagent` field in `invoke_subagent` MUST NOT bypass the allowlist (no fallback to `task` unless `task` is explicitly allowed)
- Existing subagent behavior (no delegation) MUST NOT change for subagents without the `subagents` field
- Existing named-subagent prompt behavior MUST NOT change for user/project subagents unless they opt into the new `context` policy
- If a subagent has `invoke_subagent` in tools but no `subagents` field, engine should log a warning (fail-open is intentional but must be visible)
- `task` MUST be suppressed from the `Invoke_Subagent` schema enum unless explicitly in the allowlist — `supported_subagent_names()` currently always injects it
- Restricted subagents MUST see allowlist-consistent `Invoke_Subagent` descriptions/help text, not generic guidance mentioning disallowed options

# Key decisions (decide early)
- **`--as-subagent` for named subagents only**: `task` (default alias) has no definition file, so it continues using the current explicit-flags path (`--effective-system-prompt-file` + `--tools` + `-m`). Named subagents use `--as-subagent` exclusively.
- **Behavior when `subagents` field is absent**: no restriction (backward-compatible, preserves `task` behavior). Empty list `subagents: []` = `invoke_subagent` tool available but no subagents allowed (effectively useless, but explicit).
- **Behavior when `context` field is absent**: default to all false for compatibility. Built-ins opt in explicitly.
- **MVP `context` shape**: keep it narrow — only `environment` and `project_context`. `project_context` includes scoped path references. Add more toggles later only if needed.
- **Context implementation point**: append inherited context in `subagents::render_prompt()`, not the generic standalone renderer.
- **User/project subagent recursion**: user-defined subagents in `.zdx/subagents/` can already add `invoke_subagent` to their tools. Once this ships, they can delegate — cycles are possible unless separately guarded. This is acknowledged as out-of-scope for MVP but a known risk.

# Testing
- Manual smoke demos per slice
- Unit tests for `subagents` allowlist filtering and enforcement (Slice 1)
- Unit tests for `--as-subagent` identity resolution and flag conflicts (Slice 2)
- Integration test: subagent with restricted `subagents` list cannot invoke disallowed subagents

# Polish phases (after MVP)

## Phase 1: Prompt refinement
- Tune oracle prompt based on real delegation patterns
- Adjust when-to-delegate heuristics based on observed over/under-delegation
- ✅ Check-in demo: review 5+ real delegation chains for quality

## Phase 2: Observability
- Log delegation chains (parent → child → grandchild) for debugging
- Surface delegation depth in thread metadata
- ✅ Check-in demo: thread transcript shows delegation chain clearly

# Later / Deferred
- **Explorer → Thread Searcher**: deferred because `thread-searcher` has `bash` which breaks the read-only boundary. Revisit when: (a) thread-searcher's bash is replaced with a narrow `zdx threads tools` engine tool, or (b) a read-only thread-audit subagent is created
- **Generic depth counter**: add when user-defined subagents can delegate freely or cycles are possible
- **User-defined delegation graphs**: user/project subagents can already add `invoke_subagent` — acknowledge this as a known risk, guard with depth counter when needed
- **Thread-searcher delegation**: no clear use case yet; revisit if thread-searcher needs codebase context
- **Bidirectional oracle ↔ explorer**: revisit only if real use cases emerge; currently the DAG is sufficient
