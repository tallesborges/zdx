# Goals
- A multi-agent orchestrator in zdx-core that alternates between 2+ LLM models on a shared transcript
- Presets define the orchestration shape: `brainstorm` (open discussion), `review` (critique/feedback), etc.
- The LLM triggers orchestration via bash tool (taught by skills) — not manually invoked
- Output streams to stdout for the calling agent to consume and summarize naturally

# Non-goals
- Real-time parallel agent execution (agents run sequentially)
- Dynamic turn stopping / convergence detection (fixed turns only for MVP)
- TUI-native orchestrator rendering (TUI integration deferred after CLI proves the shape)
- Built-in summarizer (the calling agent summarizes naturally)
- Thread persistence for orchestrator output (MVP is ephemeral stdout; persistence deferred)

# Design principles
- User journey drives order
- CLI-first: prove the core loop before adding TUI complexity
- Reuse existing infrastructure: `run_turn`, provider resolution, context loading
- Keep `zdx-core` UI-agnostic: orchestrator lives in core, rendering in CLI/TUI
- Orchestrator is the engine; presets (brainstorm, review) are config, not code
- Subagent = `zdx exec`; orchestrator = composition of `run_turn` calls; skills = glue that teaches the LLM when to use each

# User journey
1. User is chatting with an agent and says "brainstorm with codex about how to implement feature X"
2. The agent (taught by a skill) runs `zdx orchestrate "..." --agents ... --turns 3` via bash tool
3. Agent A responds to the topic
4. Agent B responds, seeing Agent A's message
5. Alternation continues for N rounds
6. Full discussion output returns to the calling agent via bash stdout
7. The calling agent summarizes the brainstorm naturally in its response

Alternative: user runs `zdx orchestrate` directly from terminal for standalone use.

# Foundations / Already shipped (✅)

## `zdx exec` as subagent primitive
- What exists: `zdx exec -p "prompt" -m model` runs a prompt with tool access and isolated context. The LLM can call it via bash tool for research, analysis, or any isolated task.
- ✅ Demo: `zdx exec -p "research how Ratatui handles scrolling" -m gemini:gemini-2.5-pro`
- Gaps: Creates a thread by default (use `--no-thread` for true isolation). Full tool access by default (use `--no-tools` to restrict). These are features, not bugs — but skills should document the right flags.
- Note: Existing subagent callers (handoff, thread title, read_thread) use `std::env::current_exe()` for PATH safety (`runtime/handoff.rs:69`, `runtime/thread_title.rs:57`, `tools/read_thread.rs:106`). Skills invoking via bash should use the full path or rely on PATH.

## Provider resolution & `run_turn`
- What exists: `resolve_provider` handles model string → provider routing (`providers/mod.rs`); `run_turn` drives a single provider turn with streaming, interrupts, and tool loop (`core/agent.rs:491`); takes `system_prompt: Option<&str>` passed to provider's `send_messages_stream`
- ✅ Demo: `zdx exec -p "hello" -m claude-cli:claude-sonnet-4-20250514` works
- Gaps: `run_turn` couples provider construction + turn execution; orchestrator needs to construct multiple providers and run turns with per-agent config overrides (model, system prompt). See Key Decisions.
- **Important**: `run_turn` does not load AGENTS.md/skills context itself. The caller must build the system prompt including project context (see `exec.rs:52` for reference). Orchestrator must do the same.

## System prompt chain
- What exists: `run_turn` passes `system_prompt` to providers. Providers use the caller-composed prompt directly.
- ✅ Demo: system prompt flows through to all providers
- Gaps: none (provider-specific static coding preludes removed in favor of unified templated prompt assembly)

## Subagent pattern
- What exists: Handoff, thread title, and `read_thread` all use one-shot subagent calls
- ✅ Demo: Thread auto-titling runs after first turn
- Gaps: None — can reference as patterns for future orchestrator enhancements

## No-tools mode
- What exists: `ToolSelection::Explicit(Vec::new())` disables tools (used by `exec --no-tools`)
- ✅ Demo: `zdx exec --no-tools -p "hello"` works without tool definitions
- Gaps: None
- Note: Available for later use if a `--no-tools` flag is added to orchestrator, but MVP uses default tool config per provider

# MVP slices (ship-shaped, demoable)

## Slice 1: Orchestrator engine + CLI command
- **Goal**: Core orchestrator loop in zdx-core + `zdx orchestrate` CLI command with labeled streaming output
- **Scope checklist**:
  - [ ] New module `crates/zdx-core/src/core/orchestrator.rs`
  - [ ] `OrchestratorConfig` struct: topic, list of `AgentSlot` (model string + optional display name), number of rounds, preset enum
  - [ ] `AgentSlot` struct: `model: String`, `label: String` (derived from model string if not provided)
  - [ ] `OrchestratorEvent` enum that wraps `AgentEvent` with agent identity:
    ```
    enum OrchestratorEvent {
        AgentTurnStarted { agent_index: usize, label: String },
        AgentEvent { agent_index: usize, label: String, event: AgentEvent },
        AgentTurnCompleted { agent_index: usize, label: String, text: String },
        Completed,
    }
    ```
    Note: `AgentTurnCompleted` carries only the final text (not the full `messages` vec from `TurnCompleted`) to avoid bloating event traffic. The orchestrator manages the shared transcript internally. Tool events (ToolRequested, ToolCompleted, etc.) flow through `AgentEvent` naturally — the CLI renderer can display them or ignore them.
  - [ ] `run_orchestrator` async function that:
    - Loads project context (AGENTS.md, skills) same as `exec` mode does (`exec.rs:52`)
    - For each round, for each agent slot: builds a temporary `Config` with that agent's model, calls `run_turn` with default tool config and orchestrator system prompt
    - Encodes other agents' responses as user-role messages with text prefix `[Agent <label>]: <content>`
    - Emits `OrchestratorEvent`s through a channel
    - Each agent's own responses come back as `assistant` role (natural from `run_turn`)
  - [ ] Orchestrator system prompt per preset:
    - `brainstorm`: "You are participating in a brainstorm with other AI agents. Respond constructively and concisely. Build on others' ideas or respectfully challenge them."
    - `review`: "You are reviewing an idea proposed by another AI agent. Provide constructive critique, identify risks, and suggest improvements."
  - [ ] Require `--agents` values to use provider prefixes (e.g. `claude-cli:claude-sonnet-4-20250514` or `openrouter/model-name`). Validate using `resolve_provider` — reject if resolved provider is the default fallback (Anthropic) and model doesn't match Anthropic heuristics. Validate ≥2 agents, turns ≥1.
  - [ ] Wire into `crates/zdx-core/src/core/mod.rs` exports
  - [ ] New subcommand in `crates/zdx-cli/src/cli/commands/orchestrate.rs`
  - [ ] Args: positional topic, `--agents` (comma-separated prefixed model strings, required), `--turns` (default 3), `--preset` (default `brainstorm`, options: `brainstorm`, `review`)
  - [ ] Validate `--agents` has ≥2 entries, all with explicit provider prefix (`:` or `/` separator, matching `resolve_provider` parsing in `providers/mod.rs:206`)
  - [ ] Streaming renderer in `crates/zdx-cli/src/modes/orchestrate.rs`:
    - On `AgentTurnStarted`: print `\n--- Agent: <label> (round N/M) ---\n`
    - On `AgentEvent(_, AssistantDelta)`: print text chunk to stdout
  - [ ] Wire into CLI dispatch
- **✅ Demo**: `zdx orchestrate "What are the top 3 features to build next?" --agents claude-cli:claude-sonnet-4-20250514,codex:gpt-5.3-codex --turns 1` prints labeled alternating agent responses to stdout
- **Risks / failure modes**:
  - Role encoding: providers drop unknown roles. Mitigation: always use `user`/`assistant` roles only, encode speaker in text prefix
  - System prompt quality depends on the unified template content. Mitigation: keep orchestrator instructions explicit and concise.
  - Context loading: must explicitly load AGENTS.md + skills context like `exec` mode does, or agents won't have project context.
  - `run_turn` re-creates the provider client on every call. For MVP this is fine (stateless HTTP clients). If it becomes a perf issue, refactor later.
  - API key requirements: both providers need valid auth. Mitigation: fail fast with clear error per agent before starting.
  - Unprefixed model strings silently route to Anthropic (`resolve_provider` default). Mitigation: CLI validates prefix presence.

## Slice 2: Skills (no code — just SKILL.md files)
- **Goal**: Teach the LLM when and how to trigger orchestration and subagent patterns
- **Scope checklist**:
  - [ ] Create `~/.zdx/skills/orchestrate/SKILL.md`:
    - Name: `orchestrate`
    - Description: "Trigger multi-agent brainstorm or review sessions when the user asks for a second opinion, brainstorm, or code review with another model."
    - Instructions: document `zdx orchestrate` usage, flags, example invocations
    - Include guidance on which agent combos work well
  - [ ] Create `~/.zdx/skills/subagent/SKILL.md`:
    - Name: `subagent`
    - Description: "Delegate isolated research or analysis tasks to a separate zdx instance when you need to keep the current context clean."
    - Instructions: document `zdx exec -p "..." -m model --no-thread` usage pattern
    - Include guidance on when to delegate vs do inline
  - [ ] Skills must be in `~/.zdx/skills/` (user-level) or `.zdx/skills/` (project-level) — these are the discovered source directories
- **✅ Demo**: Start a chat, ask "brainstorm with codex about X" — the agent reads the skill and runs `zdx orchestrate` via bash
- **Risks / failure modes**:
  - LLM may not reliably follow skill instructions. Mitigation: iterate on SKILL.md wording. If unreliable, upgrade to a dedicated tool later.
  - PATH issues: LLM invokes `zdx` via bash which depends on PATH. Mitigation: skill instructions can suggest using absolute path if needed.

# Contracts (guardrails)
- Each agent must see the complete prior transcript (no message dropping)
- `zdx exec` and interactive TUI behavior must not regress
- Interrupts (Ctrl+C) must cleanly stop the orchestrator loop
- `--agents` must require provider-prefixed model strings (`:` or `/` separator)

# Key decisions (decide early)

- **Naming**: `orchestrate` is the command, `orchestrator` is the engine. Presets (`brainstorm`, `review`) define behavior via config, not separate commands.
- **Subagent primitive**: `zdx exec` is already shipped and works. No new subagent code needed — just a skill to teach the LLM when to use it.
- **Invocation model**: LLM-triggered via bash tool (taught by skills). Direct CLI use also works but is secondary.
- **No built-in summarizer**: The calling agent naturally summarizes the orchestrator output. No separate summarizer turn needed in MVP.
- **No thread persistence for orchestrator**: MVP output is ephemeral (stdout). The orchestration result lives in the parent thread as bash tool output. Dedicated persistence deferred.
- **Role encoding**: Use `user` role for other agents' messages in model input, with text prefix `[Agent <label>]: <content>`. This is the safest approach across all providers.
- **Tools (KISS)**: Agents use default tool config per provider — same as regular `zdx exec`. No special restriction. If agents want to read files or run commands during brainstorm, that's fine. A `--no-tools` flag can be added later if needed.
- **Sequential execution**: Agents run one at a time. Deterministic transcript ordering.
- **System prompt (MVP compromise)**: Many providers (OpenAI-compatible, Gemini, Claude CLI, Codex) prepend coding-focused system prompts. Anthropic API and OpenAI API do not. MVP accepts this inconsistency — the orchestrator's brainstorm/review prompt is appended as the user-provided system prompt. For providers that prepend coding context, this is actually useful when brainstorming about code. Refactoring all providers to support a "raw prompt" mode is deferred to Polish Phase 1.
- **Context loading**: Orchestrator must load AGENTS.md + skills context explicitly (same as `exec` mode), not rely on `run_turn` to do it.
- **Event wrapper**: `OrchestratorEvent` wraps `AgentEvent` with agent identity (`agent_index`, `label`). This solves the "no speaker field" problem without modifying `AgentEvent` itself.
- **Provider prefix required**: `--agents` values must use explicit provider prefixes (`:` or `/` separator, e.g. `claude-cli:claude-sonnet-4-20250514` or `openrouter/model`). Prevents silent misrouting via `resolve_provider` defaults.
- **Config override per agent**: The orchestrator creates a modified `Config` clone per agent slot, overriding `model` field. All other config (max_tokens, thinking_level) comes from the user's global config. Per-agent config overrides are deferred.
- **Testing**: Manual smoke testing for MVP. Automated integration tests deferred to polish.

# Testing
- Manual smoke demos per slice (run real orchestrations, verify output visually)
- `cargo clippy` and `cargo test --workspace --lib --tests --bins` must pass (no regressions)
- Automated orchestrator-specific tests deferred to Polish Phase 3

# Polish phases (after MVP)

## Phase 1: System prompt control
- Add a `raw_system_prompt` or `prompt_mode` parameter to provider `send_messages_stream` that skips the coding prelude
- Or: add a `PromptMode::Orchestrator` that uses a minimal system prompt without coding instructions
- ✅ Check-in demo: orchestrator brainstorm without coding prelude in system prompt

## Phase 2: Summarizer turn
- After orchestration rounds complete, optionally run a summarizer agent
- `--summarizer` flag specifies model; summarizer sees full transcript
- Summarizer system prompt: "Summarize this multi-agent discussion into: 1) Key decisions 2) Action items 3) Open questions. Be concise."
- ✅ Check-in demo: `zdx orchestrate "topic" --agents A,B --turns 2 --summarizer claude-cli:claude-sonnet-4-20250514` prints discussion then summary

## Phase 3: Automated tests
- CLI integration tests with wiremock: verify event sequence, agent labels, turn ordering
- Regression tests for transcript output format
- ✅ Check-in demo: `cargo test` includes orchestrator-specific tests

## Phase 4: Thread persistence
- Create a `ThreadLog` for orchestrator sessions with meta title: `[brainstorm] <topic>`
- Save each agent's message as `ThreadEvent::Message` with agent label in text
- `zdx threads show <ID>` works for orchestrator threads (reference-only, no resume)
- ✅ Check-in demo: `zdx threads list` shows orchestrator threads, `zdx threads show <ID>` replays transcript

## Phase 5: TUI integration
- Command palette `/orchestrate` or `/brainstorm` command
- Color-coded agent labels in transcript (each agent gets a distinct color)
- Ability to orchestrate from current thread context
- ✅ Check-in demo: trigger orchestration from TUI, see color-coded multi-agent output

## Phase 6: Advanced orchestration
- Configurable turn strategies (round-robin, directed, free-form)
- Early stopping when agents converge
- More than 2 agents
- Agent personas/roles ("you are the skeptic", "you are the optimist")
- Per-agent config overrides (different thinking levels, max tokens)
- ✅ Check-in demo: 3-agent brainstorm with different personas, stops early on consensus

## Phase 7: Review preset
- `zdx orchestrate --preset review` with review-specific system prompts
- Pre-loaded context from current diff or thread
- Structured output (approve/reject/suggestions)
- ✅ Check-in demo: `zdx orchestrate --preset review --context-from-diff HEAD~1` runs a code review

# Later / Deferred
- **Dedicated `orchestrate` tool**: structured tool call instead of bash — triggers if skills are unreliable
- **`--no-tools` flag**: disable tools for orchestrator agents — triggers if tool use during brainstorm causes unwanted side effects
- **Parallel agent execution**: run agents simultaneously and merge — triggers when latency becomes a pain point
- **Persistent orchestration configs**: save agent combos + presets in config.toml — triggers when users have preferred setups
- **Thread meta `kind` field**: add optional `kind: "orchestrate"` to `ThreadEvent::Meta` for filtering — triggers when users want to filter threads by type
- **Cost tracking**: per-agent token usage reporting — triggers when users care about cost optimization
- **Shortcut aliases**: `zdx brainstorm` as alias for `zdx orchestrate --preset brainstorm` — triggers after usage proves the preset model works
