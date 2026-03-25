# Goals
- Keep MCP support inside ZDX as an internal engine for connecting to MCP servers over `stdio` and `http`.
- Expose MCP to users primarily through a skill-facing `zdx mcp ...` helper CLI, not by dumping large MCP tool catalogs directly into the model tool list by default.
- Make MCP-backed skills feel like normal bash/CLI workflows for the agent.
- Preserve warm MCP connections for the lifetime of the current run/session/process where that improves usability and latency.

# Non-goals
- Exposing full MCP server tool catalogs directly to the model by default in normal ZDX usage.
- MCP resources, prompts, sampling, elicitation, or approval UX.
- Interactive MCP dashboards or heavy management UI.
- Replacing native ZDX tools with MCP-backed equivalents.

# Design principles
- User journey drives order
- Skills over tool explosion
- Reuse the existing MCP engine; change the product surface, not the protocol plumbing
- Match the agent’s strengths: shell commands, scripts, and focused CLIs
- Keep MCP session lifetime scoped to the current run/session/process

# User journey
1. User configures MCP servers once using the supported `mcpServers` config shape.
2. User starts `zdx`, `zdx exec`, or the bot, and ZDX initializes MCP connections for that run/session/process.
3. A skill or bash workflow calls `zdx mcp ...` to inspect servers, list tools, inspect schemas, or call a tool.
4. The model uses the helper CLI as part of a skill-driven workflow instead of seeing every MCP tool as a first-class model tool.
5. MCP connections stay alive for the lifetime of the current run/session/process so repeated MCP operations stay fast and stateful enough for practical use.

# Foundations / Already shipped (✅)
List capabilities that already exist and should not be rebuilt.

## Core MCP engine
- What exists: `crates/zdx-core/src/mcp.rs` already parses the standard `mcpServers` JSON shape, supports `stdio` and `http`, discovers tools, and routes tool calls through MCP.
- ✅ Demo: `load_workspace(root)` loads `.mcp.json`, initializes configured servers, and returns workspace status plus diagnostics.
- Gaps: Product surface still assumes direct model tool exposure instead of a skill-facing helper CLI.

## Default surface rollback + helper CLI foundation
- What exists: direct MCP-to-model exposure has been removed from the default `exec`, TUI, and bot surfaces, and a dedicated `zdx mcp` helper CLI now exposes server/tool inspection and tool calls.
- ✅ Demo: built-in tool lists stay unchanged in normal agent turns, while `zdx mcp servers|tools|schema|call` works against the same core MCP engine.
- Gaps: warm connection reuse across repeated helper invocations and long-lived app sessions is still deferred.

## Diagnostics + timeouts
- What exists: MCP diagnostics are surfaced, and connect/discovery/tool-call timeouts already exist in the core MCP module.
- ✅ Demo: MCP config load failures and server failures produce structured summaries rather than crashing the whole app.
- Gaps: No operator-friendly CLI for checking status or inspecting MCP state directly.

## Stable naming + failure isolation
- What exists: MCP tools already get stable collision-safe names, and per-server discovery failures are isolated.
- ✅ Demo: one failing server does not prevent other servers from loading.
- Gaps: those names are currently useful mostly for direct model tool exposure, not for a human/skill-facing command surface.

# MVP slices (ship-shaped, demoable)
Define Slice 1..N in user-journey order.

## Slice 1: Keep MCP internal, stop treating direct model exposure as the main product
- **Goal**: Reframe MCP as internal infrastructure for skills/CLI workflows rather than the primary user-facing tool model.
- **Scope checklist**:
  - [x] Lock the product direction: MCP remains in `zdx-core`, but the preferred UX is `zdx mcp ...` + skills.
  - [ ] Keep session-scoped MCP loading in `exec`, TUI, and bot so the engine stays ready for repeated calls.
  - [x] Document that native ZDX tools remain the primary model-visible tools.
  - [x] Treat direct MCP-to-model exposure as optional/deferred rather than the default path.
- **✅ Demo**: The plan, spec direction, and architecture docs all describe MCP as an internal engine + helper CLI for skills, not as “load every MCP tool into the agent by default.”
- **Risks / failure modes**:
  - Carrying both product directions at once creates confusion.
  - Existing implementation momentum may bias the UX back toward direct model exposure.

## Slice 2: Add a minimal `zdx mcp` helper CLI
- **Goal**: Give skills and operators a small, stable CLI for interacting with MCP without exposing all MCP tools to the model.
- **Scope checklist**:
  - [x] Add `zdx mcp servers` to list configured servers and status/diagnostics.
  - [x] Add `zdx mcp tools <server>` to list tools for a configured server.
  - [x] Add `zdx mcp schema <server> <tool>` to print the input schema/details for one tool.
  - [x] Add `zdx mcp call <server> <tool> --json '{...}'` to execute a tool and return structured output.
  - [x] Ensure outputs are script-friendly so skills can use them from bash.
- **✅ Demo**: A skill can shell out to `zdx mcp tools figma`, inspect a schema, and call one MCP tool through `zdx mcp call ...` without native model tool exposure.
- **Risks / failure modes**:
  - CLI output becomes too human-oriented and hard for skills to parse.
  - The command surface grows too large and recreates a generic MCP client instead of a focused helper.

## Slice 3: Preserve warm MCP connections for the current execution lifetime
- **Goal**: Keep MCP connections fresh for the lifetime of the current run/session/process so repeated commands are fast and less flaky.
- **Scope checklist**:
  - [ ] Reuse per-run registry/clients in `exec`.
  - [ ] Reuse per-session registry/clients in TUI.
  - [ ] Reuse per-process registry/clients in the bot.
  - [ ] Add lightweight status inspection so skills/operators can tell whether a server is loaded or failed without a “probe tool” workaround.
  - [ ] Define what happens when the active root changes in TUI or when config changes require a refresh.
- **✅ Demo**: In TUI or bot, multiple `zdx mcp call ...` operations against the same server in one session do not require cold initialization every time, and `zdx mcp servers` clearly reports loaded/failed state.
- **Risks / failure modes**:
  - Long-lived connections become stale with no refresh path.
  - Ad-hoc status checks accidentally reconnect or perturb healthy sessions.

## Slice 4: Turn MCP-backed workflows into skills
- **Goal**: Validate the product direction by using MCP through skills instead of direct tool exposure.
- **Scope checklist**:
  - [ ] Create or adapt one skill that uses `zdx mcp ...` for a server that is MCP-first (for example Figma).
  - [ ] Prefer direct CLI/API skills where MCP is unnecessary (for example Sentry CLI or native Apple CLIs for non-IDE workflows).
  - [ ] Document the recommended pattern: native ZDX tools + direct CLI/API skills + MCP-backed helper CLI only where needed.
  - [ ] Confirm the model can follow the helper CLI workflow with simple skill instructions.
- **✅ Demo**: A Figma-oriented skill can use `zdx mcp` commands from bash successfully, while Sentry and routine Xcode workflows continue to use direct CLI/API paths.
- **Risks / failure modes**:
  - Skills may still need too much MCP-specific instruction if the helper CLI surface is awkward.
  - Users may expect every MCP server to become a first-class agent tool automatically.

# Contracts (guardrails)
List non-negotiable behaviors that must not regress (derived from Inputs and existing behavior).
- ZDX must accept the standard `mcpServers` config shape for the supported MCP config source.
- MCP server failures must stay isolated per server.
- Native ZDX tools remain the primary model-visible tool surface.
- MCP helper commands must be script-friendly so skills can use them reliably.
- MCP connections should stay warm only for the lifetime of the current run/session/process, not become hidden global daemons by default.

# Key decisions (decide early)
List only decisions that would cause rework if postponed (derived from Inputs).
- Direct MCP-to-model exposure is removed from the default UX; any future opt-in/debug path is deferred.
- MVP helper CLI taxonomy is `servers`, `tools`, `schema`, and `call`.
- MVP helper CLI defaults to JSON output.
- Whether MCP config remains project-local `.mcp.json`, global-only, or supports both with precedence rules.

# Testing
- Manual smoke demos per slice
- Minimal regression tests only for contracts
- Validate helper CLI output shape for scripting
- Validate session reuse behavior separately for `exec`, TUI, and bot

# Polish phases (after MVP)
Group improvements into phases, each with a ✅ check-in demo.
Limited strictly to scope present in Inputs.

## Phase 1: Better operator ergonomics
- Add clearer `zdx mcp servers` status output (loaded, failed, timed out, stale).
- Add richer diagnostics for auth, transport, and timeout failures.
- ✅ Check-in demo: a broken Figma or Sentry MCP config is easy to diagnose without opening logs or guessing.

## Phase 2: Optional focused CLI generation
- Explore whether `zdx mcp` should generate focused wrapper scripts/binaries for specific servers or tool subsets.
- Keep this tightly scoped to skills usage, not as a full generic codegen product.
- ✅ Check-in demo: a skill can use a tiny generated wrapper for one MCP-backed workflow if that proves simpler than repeated `zdx mcp call ...` commands.

# Later / Deferred
Explicit list of "not now" items + what would trigger revisiting them.
- Full direct MCP tool exposure to the model — revisit only if the skills/CLI surface proves too awkward or too limiting.
- MCP resources/prompts/sampling — revisit only after helper CLI + skills usage is stable.
- Heavy MCP UI/management dashboards — revisit only if operator diagnostics from the helper CLI are insufficient.
- Standalone MCP codegen product ambitions — revisit only if a minimal wrapper-generation workflow proves genuinely useful.