# Goals
- Restructure the workspace into cleaner layered boundaries: `zdx-assets`, `zdx-types`, `zdx-providers`, `zdx-tools`, and `zdx-engine`.
- Keep ZDX usable throughout the migration: TUI, exec mode, bot, threads, and tool behavior must keep working slice by slice.
- Make crate names and crate contents reflect real ownership, so it is obvious where new code belongs.

# Non-goals
- Product behavior redesign.
- Plugin/dynamic-loading architecture.
- Big-bang deletion of `zdx-core`.
- Feature expansion beyond what is needed to preserve current behavior during the refactor.

# Design principles
- User journey drives order
- Ship-first: every slice leaves the repo runnable and demoable
- Real boundaries over naming symmetry
- Keep `zdx-types` pure and small
- Prefer a temporary facade over broad import churn

# User journey
1. Developer can still build, run, and test ZDX normally while the redesign is underway.
2. Developer can predict where new prompts, shared types, providers, tools, and engine features belong.
3. Surface crates keep using a stable API while internals move behind a compatibility layer.
4. Only after the new boundaries are proven do surfaces cut over from `zdx-core` to the new crates.

# Foundations / Already shipped (✅)
List capabilities that already exist and should not be rebuilt.

## Runnable multi-surface product
- What exists: TUI, `zdx exec`, bot, and monitor already share runtime behavior successfully.
- ✅ Demo: `just run`, `zdx exec -p "hello"`, and bot commands still start from the workspace.
- Gaps: crate boundaries do not clearly express ownership.

## Append-only threads and deterministic tools
- What exists: thread persistence, thread replay/search, and tool execution are already part of the product contract.
- ✅ Demo: create a thread, run a tool-backed prompt, replay/search it.
- Gaps: provider, tool, and thread-related contracts are still interwoven in `zdx-core`.

## Embedded assets pipeline
- What exists: prompts, subagents, bundled skills, and default TOMLs are embedded with `include_str!`, `include_bytes!`, and `build.rs`.
- ✅ Demo: clean build still loads prompts and materializes bundled skills.
- Gaps: asset ownership is mixed into crate roots and is harder to reason about than it should be.

# Target dependency graph
- `zdx-types` → no local crate dependencies
- `zdx-assets` → no local crate dependencies
- `zdx-providers` → may depend on `zdx-types`
- `zdx-tools` → may depend on `zdx-types`
- `zdx-engine` → may depend on `zdx-types`, `zdx-assets`, `zdx-providers`, and `zdx-tools`
- `zdx-core` (temporary facade) → may depend on every new runtime crate and re-export selected APIs
- Surface crates (`zdx-cli`, `zdx-tui`, `zdx-bot`, `zdx-monitor`) → keep depending on `zdx-core` until Slice 6

## Forbidden dependency directions
- `zdx-types` must not depend on any local crate.
- `zdx-assets` must not depend on runtime crates.
- `zdx-providers` must not depend on `zdx-tools` or `zdx-engine`.
- `zdx-tools` must not depend on `zdx-providers` or `zdx-engine`.
- `zdx-engine` is the only crate allowed to compose providers + tools + assets + config/runtime state.

# Current module → target crate map

## `zdx-assets`
- `prompts/`
- `bundled_skills/`
- `subagents/`
- `default_config.toml`
- `default_models.toml`
- `build.rs` asset-manifest logic for bundled skills

## `zdx-types`
- shared provider/tool/thread/event DTOs and enums currently spread across `providers`, `tools`, `core::events`, and `core::thread_persistence`

## `zdx-providers`
- `src/providers/`

## `zdx-tools`
- `src/tools/` infrastructure and leaf tools only

## `zdx-engine`
- `src/core/`
- `config.rs`
- `models.rs`
- `skills.rs`
- `subagents.rs`
- `automations.rs`
- `mcp.rs`
- `agent_activity.rs`
- `audio/`
- `images/`
- `pidfile.rs`
- `tracing_init.rs`
- engine-owned tool adapters still exposed to the model

## `zdx-core` (temporary facade)
- `src/lib.rs` becomes re-exports only
- keep the minimum compatibility surface needed by CLI/TUI/Bot/Monitor until Slice 6

# `zdx-core` temporary facade surface

## Compatibility-first rule
- During Slices 1–5, prefer updating `zdx-core` internals and re-exports before migrating surface crates.
- Surface crates should continue compiling against `zdx-core` until Slice 6 unless a direct import clearly reduces churn.

## APIs the facade must continue exposing until Slice 6
- `config`
- `core::agent`
- `core::context`
- `core::events`
- `core::interrupt`
- `core::subagent`
- `core::thread_persistence`
- `core::title_generation`
- `core::worktree`
- `providers`
- `tools`
- `prompts`
- `models`
- `skills`
- `subagents`
- `automations`
- `mcp`
- `agent_activity`
- `audio`
- `images`
- `pidfile`
- `tracing_init`

# `zdx-types` inventory

## Move into `zdx-types`
- provider-facing shared message types such as `ChatMessage`, `ChatContentBlock`, `MessageContent`, `ReasoningBlock`, and `ReplayToken`
- provider/tool shared enums such as `ProviderKind`, `ProviderAuthMode`, `ProviderErrorKind`, `ThinkingLevel`, and `TextVerbosity` only if they are used as pure value types
- agent/tool event value types such as `AgentEvent`, `ErrorKind`, `TurnStatus`, `ToolOutput`, `ToolResult`, `ToolResultContent`, `ToolResultBlock`, and tool-definition data shapes
- thread schema value types such as `ThreadEvent`, `Usage`, `ThreadSummary`, and related search-result structs if they are plain data

## Keep out of `zdx-types`
- `Config`, `ProvidersConfig`, path helpers, and any loader/saver code
- `ToolContext`, `ToolRegistry`, executors, and registry composition
- provider HTTP clients, OAuth helpers, and streaming/parsing implementations
- thread persistence I/O, search implementation, or transcript formatting
- prompt rendering, skill loading, subagent discovery, and model registry loading
- MCP runtime, automations runtime, and worktree/runtime helpers

# Tool classification

## Leaf tools that belong in `zdx-tools`
- `bash`
- `apply_patch`
- `edit`
- `write`
- `read`
- `glob`
- `grep`
- `web_search`
- `fetch_webpage`

## Engine-backed tool adapters that stay with `zdx-engine` first
- `read_thread`
- `invoke_subagent`
- `thread_search`
- `todo_write` (because it currently mutates thread state via thread persistence)

## Optional later reclassification
- After Slice 6, revisit whether engine-backed tools should move physically into `zdx-tools` behind explicit engine service traits.
- Do not block the redesign on this abstraction.

# MVP slices (ship-shaped, demoable)
Define Slice 1..N in user-journey order.

## Slice 1: Create `zdx-assets`
- **Goal**: Move embedded assets into a dedicated owner crate without changing runtime behavior.
- **Scope checklist**:
  - [ ] Add `crates/zdx-assets`.
  - [ ] Move core prompts, bundled skills, subagents, `default_config.toml`, and `default_models.toml` into it.
  - [ ] Move recursive asset manifest/build logic there.
  - [ ] Update any direct `include_str!`/`include_bytes!` callers to use `zdx-assets` accessors.
  - [ ] Re-export asset accessors from `zdx-core` so surfaces do not change yet.
  - [ ] Update `docs/ARCHITECTURE.md`, root `AGENTS.md`, and any affected scoped `AGENTS.md` files in the same slice.
- **✅ Demo**: prompts and bundled skills still resolve at runtime; `zdx`, `zdx exec`, and bot flows behave the same.
- **Risks / failure modes**:
  - Broken embed paths.
  - Bundled-skill materialization regressions.

## Slice 2: Create `zdx-types`
- **Goal**: Extract pure shared contracts before moving runtime-heavy code.
- **Scope checklist**:
  - [ ] Move the first wave of pure DTOs/enums used across providers, tools, thread persistence, and events.
  - [ ] Start with provider message/value types and `core::events` value types before touching thread persistence shapes.
  - [ ] Move thread-schema value types only after they no longer depend on config/provider implementation modules.
  - [ ] Keep config, path helpers, loaders, and runtime services out of `zdx-types`.
  - [ ] Re-export types from `zdx-core` to keep downstream imports stable.
  - [ ] Update docs/AGENTS affected by any moved file/module names in the same slice.
- **✅ Demo**: thread replay and provider/tool protocol behavior stay unchanged; no local dependency cycles appear.
- **Risks / failure modes**:
  - Turning `zdx-types` into a new god crate.
  - Moving I/O concerns too early.

## Slice 3: Extract `zdx-providers`
- **Goal**: Move provider implementations behind stable contracts once shared protocol types exist.
- **Scope checklist**:
  - [ ] Move provider modules and provider-specific parsers/helpers into `zdx-providers`.
  - [ ] Introduce a provider factory boundary used by the engine/facade.
  - [ ] Keep config-driven provider selection outside provider implementations.
  - [ ] Keep provider-facing tool contracts coming from `zdx-types`, not from `zdx-tools`.
  - [ ] Update docs/AGENTS in the same slice.
- **✅ Demo**: existing provider-backed runs still work with the same tool visibility, streaming behavior, and auth flows.
- **Risks / failure modes**:
  - Hidden imports from config/tools/thread modules.
  - Duplicated provider selection logic.

## Slice 4: Extract `zdx-tools` for leaf tools only
- **Goal**: Create a clean tool crate that owns tool infrastructure and generic tools.
- **Scope checklist**:
  - [ ] Move tool protocol, registry, and the leaf tools listed in the Tool classification section.
  - [ ] Keep engine-backed adapters out unless they are rewritten against explicit service traits.
  - [ ] Let the engine compose the final model-visible registry.
  - [ ] Leave `todo_write` in `zdx-engine` for this slice.
  - [ ] Update docs/AGENTS in the same slice.
- **✅ Demo**: tool lists are unchanged and leaf tools still execute from all surfaces.
- **Risks / failure modes**:
  - Forcing thread/subagent/prompt-aware tools into `zdx-tools` too early.
  - Recreating backedges into the engine.

## Slice 5: Create `zdx-engine`
- **Goal**: Make the runtime/orchestration boundary explicit.
- **Scope checklist**:
  - [ ] Move config loading, prompt assembly, thread persistence, skills, subagents, MCP, automations, and agent orchestration into `zdx-engine`.
  - [ ] Keep engine-backed tool adapters here: `read_thread`, `invoke_subagent`, and `thread_search`.
  - [ ] Keep `todo_write` here until a later abstraction removes its thread-persistence coupling.
  - [ ] Convert `zdx-core` into a compatibility facade that re-exports the new layout.
  - [ ] Move remaining utility/runtime modules here: `agent_activity`, `audio`, `images`, `pidfile`, `tracing_init`, and any leftover `core/*` helpers.
  - [ ] Update docs/AGENTS in the same slice.
- **✅ Demo**: surface crates still build against `zdx-core`, but internal ownership is now explicit and accurate.
- **Risks / failure modes**:
  - Import churn.
  - Facade gaps causing accidental API breakage.

## Slice 6: Cut over surfaces and shrink the facade
- **Goal**: Finish the redesign only after the new structure proves easier to work in.
- **Scope checklist**:
  - [ ] Migrate `zdx-cli`, `zdx-tui`, `zdx-bot`, and `zdx-monitor` to direct crate imports where it improves clarity.
  - [ ] Remove stale re-exports and dead compatibility layers.
  - [ ] Update `docs/ARCHITECTURE.md` and scoped `AGENTS.md` files to match reality.
  - [ ] Update any tests/fixtures/help output affected by import or crate-name changes.
- **✅ Demo**: the repo builds cleanly, docs match reality, and it is obvious where a new change belongs.
- **Risks / failure modes**:
  - Stopping too early and living with a confusing half-facade.

# Contracts (guardrails)
List non-negotiable behaviors that must not regress (derived from Inputs and existing behavior).
- TUI remains the primary product surface.
- `zdx exec` stdout/stderr contract stays intact.
- Append-only thread behavior and replay remain readable and stable.
- Built-in tool names and model-facing behavior stay stable unless intentionally changed.
- Bundled skills, prompts, subagents, and default TOMLs still embed/materialize deterministically.

# Key decisions (decide early)
List only decisions that would cause rework if postponed (derived from Inputs).
- `zdx-core` should become a temporary facade rather than being deleted or renamed immediately.
- `zdx-types` must stay pure from day one.
- Engine-backed tool adapters stay in `zdx-engine` unless a later trait boundary proves clearly better.
- Surface-owned instruction layers can be decided after `zdx-assets` proves the asset pattern.

# Testing
- Manual smoke demos per slice
- Minimal regression tests only for contracts
- Prefer targeted crate checks during iteration
- Finish slices with `just ci` when feasible

## Per-slice verification commands
- **Slice 1**:
  - `cargo test -p zdx-core`
  - `cargo test -p zdx-cli`
  - `zdx exec -p "hello"`
- **Slice 2**:
  - `cargo test -p zdx-core`
  - `cargo test -p zdx-tui`
  - `zdx threads list`
- **Slice 3**:
  - `cargo test -p zdx-core`
  - `cargo test -p zdx-cli`
  - one provider-backed `zdx exec -p "hello"` smoke run using the normal local config
- **Slice 4**:
  - `cargo test -p zdx-core`
  - `cargo test -p zdx-cli`
  - smoke run for `read`, `glob`, and `grep` via an agent turn or existing targeted tests
- **Slice 5**:
  - `cargo test -p zdx-core`
  - `cargo test -p zdx-tui`
  - `cargo test -p zdx-bot`
  - `zdx exec -p "hello"`
- **Slice 6**:
  - `just ci`
  - smoke `just run`
  - smoke `zdx exec -p "hello"`

## Surface impact notes
- Expect broad but temporary `use zdx_core::...` compatibility pressure: surface crates currently have ~150 direct `use zdx_core::...` imports.
- Do not start large surface import rewrites before the facade is stable.

# Polish phases (after MVP)
Group improvements into phases, each with a ✅ check-in demo.
Limited strictly to scope present in Inputs.

## Phase 1: Boundary hardening
- Reduce facade re-exports further.
- Add tiny docs/examples for where new code should go.
- ✅ Check-in demo: adding one tool, provider, and prompt no longer touches unrelated crates.

## Phase 2: Optional trait cleanup
- If still needed, introduce engine service traits so more model-visible tools can live physically in `zdx-tools` without violating boundaries.
- ✅ Check-in demo: trait-based adapters reduce direct engine imports while behavior stays unchanged.

# Later / Deferred
Explicit list of “not now” items + what would trigger revisiting them.
- Deleting `zdx-core` entirely — revisit only after surfaces run cleanly without it.
- Splitting MCP or other engine areas further — revisit only if ownership or release cadence demands it.
- Forcing all model-visible tools into one crate — revisit only if trait boundaries prove simpler than keeping engine-backed adapters in the engine.
- Any plugin/dynamic-loading architecture — revisit only if external extensibility becomes a real goal.