# Goals
- Add a first-class subagent capability so the main agent can delegate isolated tasks without bloating its own context.
- Keep subagent execution simple and reliable in MVP (single request, single response, fail-fast).
- Learn from real usage what configuration knobs are actually needed before adding them.

# Non-goals
- Multi-agent discussion/orchestration loops.
- Subagent UI management in TUI (live subagent panels, dashboards).
- Persistent subagent sessions/resume semantics.
- Automatic summarization/merging of subagent outputs.
- Profiles, `system_prompt`, `no_tools` — deferred until real usage reveals what's needed.

# Design principles
- KISS/YAGNI first: ship the smallest delegation primitive that is useful daily
- Reuse existing execution path (`zdx exec`) instead of building a second agent runtime
- Isolated by default: subagents should not pollute parent thread context
- Inherit everything from parent by default; override only what's explicitly passed

# User journey
1. User asks the main agent to delegate a scoped task (review, research, explore, etc.).
2. Main agent calls `invoke_subagent` with a task-specific prompt.
3. Subagent runs in isolation (fresh context, `--no-thread`) and returns one result.
4. Main agent continues in the same conversation, using that result.

# Foundations / Already shipped (✅)

## `zdx exec` single-shot execution
- Non-interactive execution supports prompt, model override, tool control, and `--no-thread`.
- ✅ Demo: `zdx --no-thread exec -p "hello"`

## Reusable subagent runner (`core::subagent`)
- Extracted from `read_thread` tool. Reusable `run_exec_subagent()` helper.
- ✅ Demo: `read_thread` tool uses it successfully.

## Tool system + envelopes
- Stable tool schema/validation and deterministic success/error envelopes.
- ✅ Demo: existing tools return `{ ok: true|false, ... }`.

# MVP: `invoke_subagent` tool

## Tool contract
- **Name**: `invoke_subagent`
- **Required input**: `prompt` (task-specific instructions for the subagent)
- **Optional input**: `model` (override model for this subagent run)
- **Output**: Response text on success; structured error on failure
- **Behavior**: Spawns isolated child `zdx exec` process, inherits parent tools/thinking/config
- **Config**:
  - `[subagents].enabled = true` controls whether the tool is exposed
  - `[subagents].available_models` is derived from enabled providers + model registry and restricts explicit `model` overrides
  - No `default_model`: when `model` is omitted, use current parent model

## Scope checklist
- [x] Rename tool from `subagent` to `invoke_subagent`
- [x] Required input: `prompt`
- [x] Optional input: `model` override
- [x] Remove `system_prompt` and `no_tools` params from MVP schema
- [x] Return response text only on success; clear structured error on failure
- [x] Register tool in tool registry (both full + bot tool sets)
- [x] Tool description guides when to use (review, research, explore, scoped tasks) and when NOT to use (simple questions, single-file edits)
- [x] Parallel execution works automatically (multiple `invoke_subagent` calls in one turn → `tokio::JoinSet`)

## ✅ Demo
In a normal agent run, ask "review my auth module" — agent calls `invoke_subagent` with a review prompt, gets back a fresh-context review, and summarizes it.

## Risks / failure modes
- Prompt quoting/escaping issues when spawning child process.
- Provider auth/model resolution failures from child process.
- Over-delegation can increase latency/cost.

# Contracts (guardrails)
- Subagent execution is isolated by default (`--no-thread`), parent thread context is not polluted.
- Single-shot: one prompt in, one response out.
- Fail-fast on child failure (non-zero exit, timeout, interruption, or empty result).
- Existing `zdx exec` and interactive chat behavior must not regress.

# Key decisions (decided)
- Tool name: `invoke_subagent`
- MVP schema: just `prompt` + optional `model`. No `system_prompt`, `no_tools`, or `profile`.
- Subagent inherits parent defaults (tools, thinking level, provider config).
- Enabled by default in all provider tool sets.
- Subagent config is explicit: `[subagents].enabled` + derived `[subagents].available_models`.
- If `model` is omitted, `invoke_subagent` uses current parent model.
- `available_models` applies only when `model` override is explicitly passed.
- Parallel execution: automatic via existing `tokio::JoinSet` in `execute_tools_async`.

# Testing
- Manual smoke demos
- Minimal regression tests for contracts:
  - input validation (`prompt` required)
  - child failure mapping to structured tool error
  - success path returns response text only

# Polish phases (after MVP, driven by real usage)

## Phase 1: Reliability hardening
- Clearer error classification (auth/model/timeout/interrupted).
- Improve stderr diagnostics for failed child runs.
- ✅ Demo: failing model/auth case returns actionable error.

## Phase 2: Subagent profiles
- Pre-defined profiles (e.g., `explore`, `worker`, `reviewer`) that bundle model + tools + system prompt.
- `invoke_subagent(prompt, profile)` — profile owns configuration, model only picks what to do.
- Custom agent `.md` files (Claude Code pattern): `.zdx/agents/` with YAML frontmatter.
- ✅ Demo: run two tasks with different profiles, observe different behavior.

## Phase 3: Model groups
- Semantic model slots: `fast`, `smart`, `reasoning` mapped to specific models in config.
- Profiles reference slots instead of hardcoded models.
- Switch providers in one place; profiles stay stable.
- ✅ Demo: change `fast` mapping, all explore-type subagents use new model automatically.

## Phase 4: Orchestrator patterns
- System prompt guidance for multi-subagent workflows.
- Agent decomposes large tasks into parallel `invoke_subagent` calls automatically.
- ✅ Demo: ask for a big feature, agent delegates subtasks and integrates results.

# Later / Deferred
- Real-time multi-agent debate loops; revisit when collaboration workflows become frequent.
- Nested-depth guards/recursion controls; revisit if accidental recursive delegation appears.
- TUI-native subagent timeline/panels; revisit when headless/CLI flow is proven.
- Background execution (Claude Code pattern); revisit when latency becomes a daily bottleneck.
- Agent resume by ID; revisit for long-running follow-up workflows.
- Inter-agent messaging (Claude Code SendMessage); revisit if collaborative workflows are needed.
---

# Reference: Multi-Agent Systems

> Research notes from Claude Code, Kimi K2.5, OpenAI Codex, and Letta Code.
> Sources: Claude Code system prompts (Piebald-AI/claude-code-system-prompts), Kimi K2.5 paper (arXiv 2602.02276), leaked Kimi internals (dnnyngyen/kimi-agent-internals).

## Architecture Overview

All systems use the same fundamental pattern:

```
┌──────────────────────────────────────────────────┐
│  User / Lead Agent                               │
│                                                  │
│  Decides what to parallelize, creates agents,    │
│  assigns tasks, collects results                 │
│                                                  │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐       │
│  │ Agent A  │  │ Agent B  │  │ Agent C  │       │
│  │ (own ctx)│  │ (own ctx)│  │ (own ctx)│       │
│  │ own tools│  │ own tools│  │ own tools│       │
│  └────┬─────┘  └────┬─────┘  └────┬─────┘       │
│       │              │              │             │
│       └──────────────┴──────────────┘             │
│              Results return to lead               │
└──────────────────────────────────────────────────┘
```

Key differences:
- **Claude Code**: Local CLI processes, prompt-only coordination, file-based state, peer-to-peer messaging
- **Kimi K2.5**: Server-side inference sessions, RL-trained parallelization (PARL), up to 100 sub-agents
- **OpenAI Codex**: Thread-based isolation, 4 agent role types, depth limits
- **Letta Code**: CLI process spawning (like zdx), dynamic tool description injection, stream-JSON output
- **zdx (current)**: Single `Invoke_Subagent` tool spawning child `zdx exec` process, sequential by default, parallel via `tokio::JoinSet`

---

## Claude Code Agent Teams

### How It Works

Experimental feature (enabled via `CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1`) that coordinates multiple Claude Code CLI instances. One session acts as team lead, spawning teammates that work independently in their own context windows.

Source: https://github.com/Piebald-AI/claude-code-system-prompts

### Pre-Built Agent Types (5 built-in)

#### 1. Explore — Fast Read-Only Codebase Search
- **Model**: Haiku (fast, low-latency)
- **Tools**: Read-only only (Glob, Grep, Read, limited Bash). DENIED: Write, Edit
- **Context**: Fresh slate (does NOT inherit conversation)
- **Purpose**: File discovery, code search, codebase exploration

Key system prompt excerpt:
```
You are a file search specialist. READ-ONLY MODE.
- Use Glob for broad file pattern matching
- Use Grep for searching file contents with regex
- Use Read when you know the specific file path
- Use Bash ONLY for read-only operations (ls, git status, git log, git diff, find, cat, head, tail)
NOTE: You are meant to be a fast agent. Spawn multiple parallel tool calls wherever possible.
```

#### 2. Plan — Research for Planning
- **Model**: Inherits from main conversation
- **Tools**: Read-only (denied Write and Edit)
- **Context**: Inherits full conversation

#### 3. General-Purpose — Full Capability Agent
- **Model**: Inherits from main conversation
- **Tools**: All tools (read, write, edit, bash, etc.)
- **Context**: Inherits full conversation

Key system prompt excerpt:
```
You are an agent for Claude Code. Do what has been asked; nothing more, nothing less.
When you complete the task simply respond with a detailed writeup.
- NEVER create files unless absolutely necessary. ALWAYS prefer editing existing files.
- NEVER proactively create documentation files (*.md) or README files.
- In your final response always share relevant file names and code snippets. File paths MUST be absolute.
```

#### 4. Claude Code Guide — Documentation Lookup
- **Model**: Haiku
- **Tools**: Read-only + WebFetch + WebSearch

#### 5. statusline-setup — Status Line Configuration
- **Model**: Sonnet

### Task Tool Schema (for spawning agents)

```json
{
  "description": "string (3-5 word summary)",
  "prompt": "string (task instructions)",
  "subagent_type": "string (agent type to use)",
  "model": "sonnet | opus | haiku (optional, inherits from parent)",
  "resume": "string (optional agent ID to resume)",
  "run_in_background": "boolean (optional)",
  "name": "string (optional, for team mode)",
  "team_name": "string (optional, for team mode)"
}
```

Key Task tool prompt excerpt:
```
Launch multiple agents concurrently whenever possible, to maximize performance;
to do that, use a single message with multiple tool uses.

The result returned by the agent is not visible to the user. To show
the user the result, you should send a text message back to the user
with a concise summary of the result.

You can optionally run agents in the background using the run_in_background
parameter. When an agent runs in the background, the tool result will include
an output_file path. To check on the agent's progress, use Read to read the
output file.

Agents can be resumed using the `resume` parameter by passing the agent ID
from a previous invocation.
```

### Custom Agent System (User-Defined Agents)

Claude Code supports fully custom agents as Markdown files with YAML frontmatter:

```markdown
---
name: code-reviewer
description: Reviews code for quality and best practices. Use proactively after code changes.
tools: Read, Glob, Grep, Bash
disallowedTools: Write, Edit
model: sonnet
permissionMode: dontAsk
memory: user
---
You are a senior code reviewer. When invoked, analyze the code and provide
specific, actionable feedback on quality, security, and best practices.
```

Storage locations (by priority):
1. `--agents` CLI flag (JSON) — current session only
2. `.claude/agents/` — current project
3. `~/.claude/agents/` — all projects (user-level)
4. Plugin's `agents/` directory

Configuration options: `tools`, `disallowedTools`, `model` (sonnet/opus/haiku), `permissionMode`, `memory` (user/project), `skills`

### Inter-Agent Communication (SendMessage)

5 message types: direct message, broadcast, shutdown_request, shutdown_response, plan_approval_response.

Key constraints:
- Plain text output is NOT visible to teammates — must use SendMessage tool
- Always refer to teammates by NAME, never by UUID
- Broadcast is expensive: N teammates = N deliveries
- Messages are automatically delivered

### Task Management (TaskCreate/TaskUpdate/TaskList)

```
Use TodoWrite tools VERY frequently to ensure you are tracking tasks and
giving the user visibility into your progress. If you do not use this tool
when planning, you may forget important tasks — and that is unacceptable.
Mark todos as completed as soon as you are done. Do not batch up.
```

Task states: `pending` → `in_progress` → `completed`. Only ONE task `in_progress` at a time.

### File-Based State

```
~/.claude/
├── teams/{team-name}/
│   └── config.json          # Team metadata, members array (name, agentId, agentType)
└── tasks/{team-name}/
    ├── 1.json               # Task with status, owner, deps
    └── ...
```

### Idle State Management

Teammates go idle after every turn. Sending a message to an idle teammate wakes them up. Idle notifications are automatic. When a teammate sends a DM to another, a brief summary is included in their idle notification.

### When to Use Agent Teams vs Subagents

| Aspect | Subagents | Agent Teams |
|--------|-----------|-------------|
| Context | Own context; results return to caller | Own context; fully independent |
| Communication | Report results back only | Teammates message each other directly |
| Coordination | Main agent manages all work | Shared task list with self-coordination |
| Best for | Focused tasks where only the result matters | Complex work requiring discussion |
| Token cost | Lower: results summarized back | Higher: each teammate is separate instance |

---

## Kimi K2.5 Agent Swarm

### How It Works

Trained capability in the Kimi K2.5 model (1T params MoE, 32B active). A trainable orchestrator dynamically spawns up to 100 frozen sub-agents with up to 1,500 tool calls. Trained via PARL (Parallel-Agent Reinforcement Learning).

Source: arXiv paper 2602.02276

### Tool Schemas (2 tools for orchestrator)

#### 1. create_subagent — Create a Reusable Agent Configuration

```json
{
  "name": "create_subagent",
  "parameters": {
    "name": { "type": "string", "description": "Unique name for this agent configuration" },
    "system_prompt": { "type": "string", "description": "System prompt defining the agent's role, capabilities, and boundaries" }
  },
  "required": ["name", "system_prompt"]
}
```

Backend: new K2.5 inference session initialized with custom system_prompt, access to search/browser/code tools. Configuration saved for reuse across multiple `assign_task` calls. Sub-agent is a **frozen** K2.5 instance (no PARL training).

#### 2. assign_task — Dispatch Work to a Sub-Agent

```json
{
  "name": "assign_task",
  "description": "Launch a new agent.\n1. You can launch multiple agents concurrently whenever possible;\n2. When the agent is done, it will return a single message back to you.",
  "parameters": {
    "agent": { "type": "string", "description": "Which created agent to use" },
    "prompt": { "type": "string", "description": "The task for the agent to perform" }
  },
  "required": ["agent", "prompt"]
}
```

Key: Multiple `assign_task` calls in one model turn execute **concurrently** on the backend.

### Orchestrator System Prompt

```
You are Kimi, a professional and meticulous expert in information
collection and organization.

# Available Tools
1. Search tool: supporting multiple queries in parallel.
2. Browser tools: visit web links, get page content, perform interactions.
3. Sub Agent tools:
   - 'create_subagent': Create a new sub-agent with unique name and clear system prompt.
   - 'assign_task': Delegate tasks to created sub-agents.
4. Other tools: Including code execution (IPython, Shell).
```

Note: The prompt is remarkably simple. It does NOT instruct the model to parallelize. The parallelization behavior was learned via PARL training.

### PARL Training (Parallel-Agent Reinforcement Learning)

**The Problem: Serial Collapse** — Without training, models default to sequential execution even when given parallel tools.

**The Solution: Three Reward Signals**

```
R = R_performance + α·R_anti_collapse + β·R_subtask_completion
```

1. **R_performance** — Did the overall task succeed?
2. **R_anti_collapse** — Encourages spawning multiple sub-agents (penalizes sequential). Annealed to zero over training.
3. **R_subtask_completion** — Rewards completed subtasks. Prevents "spurious parallelism" (reward hacking).

**Critical Steps Metric** — Instead of counting total steps, uses critical path analysis:

```
Critical Steps = Orchestration overhead + max(slowest subagent at each stage)
```

Spawning more sub-agents only helps if it shortens the critical path.

### Step Limits

| Benchmark | Orchestrator Max Steps | Sub-Agent Max Steps |
|-----------|----------------------|-------------------|
| BrowseComp | 15 | 100 |
| WideSearch | 100 | 100 |
| In-house Bench | 100 | 50 |

### Performance Results

| Benchmark | Single Agent | Agent Swarm | Improvement |
|-----------|-------------|-------------|-------------|
| BrowseComp | 60.6% | 78.4% | +17.8% |
| WideSearch (Item-F1) | 72.7% | 79.0% | +6.3% |

Speed: 3x–4.5x execution time reduction. Up to 80% reduction in end-to-end runtime.

### Agent Swarm as Context Management

Key insight from the paper:

> "Long-horizon tasks are decomposed into parallel, semantically isolated subtasks, each executed by a specialized subagent with a bounded local context. Only task-relevant outputs—rather than full interaction traces—are selectively routed back to the orchestrator."

This is "context sharding" not "context truncation" — scales effective context length architecturally.

### Dynamic Agent Creation Examples

From the paper's word cloud: AI Researcher, Physics Researcher, Chemistry Researcher, Biology Expert, Fact Checker, Data Analyst, Code Reviewer, Math Solver, Web Scraper, Document Analyzer...

The orchestrator writes custom system prompts for each at runtime.

---

## OpenAI Codex CLI

Source: https://github.com/openai/codex (via DeepWiki)

### Agent Roles (4 pre-built types)

| Role | Model | Instructions | Purpose |
|------|-------|-------------|---------|
| **Default** | Inherits parent | Inherits parent config | Standard agent, no overrides |
| **Orchestrator** | Inherits parent | `templates/agents/orchestrator.md` | Coordination-only, delegates to workers |
| **Worker** | Fixed override | (inheriting) | Execution and production work |
| **Explorer** | `gpt-5.1-codex-mini` | (inheriting) | Fast codebase questions, medium reasoning |

### spawn_agent Tool Schema

```json
{
  "name": "spawn_agent",
  "description": "Spawn a sub-agent for a well-scoped task. Returns the agent id.",
  "parameters": {
    "message": { "type": "string", "description": "Initial task. Include scope, constraints, and expected output.", "required": true },
    "agent_type": { "type": "string", "description": "Agent type (Default, Orchestrator, Worker, Explorer)", "required": false }
  }
}
```

### Architecture Details

- **AgentControl**: Central controller managing agent threads and inter-agent communication
- Each spawned agent runs in its own thread with a unique `ThreadId`
- `MAX_THREAD_SPAWN_DEPTH` prevents infinite recursion
- Communication via `AgentControl.send_prompt()` which sends `Op::UserInput` to a specific thread
- Agents can be resumed from rollout files via `resume_agent_from_rollout`

### Key Patterns

- Orchestrator delegates, doesn't execute
- Explorer is cheap and fast (codex-mini with medium reasoning)
- Thread-based isolation (different from Claude Code's process model and zdx's child process model)
- AgentProfile is immutable — drives configuration but can't change at runtime

---

## Letta Code

Source: https://github.com/letta-ai/letta-code (via DeepWiki)

### SubagentManager Architecture

Spawns child `letta` CLI processes in headless stream-JSON mode.

- **SubagentConfig**: Interface defining each agent type with `description`, `recommendedModel`, `memoryBlocks`, `allowedTools`
- **spawnSubagent()**: Takes `type`, `prompt`, optional `userModel`, and `subagentId`
- **executeSubagent()**: Builds CLI args via `buildSubagentArgs` and spawns a new `letta` process

### Dynamic Description Injection

**`injectSubagentsIntoTaskDescription()`**: Dynamically updates the Task tool's description with available subagent information. For each subagent, it injects name, description, and recommended model into the tool description before the "## Usage" section. This makes subagents discoverable by the main agent through the tool schema itself.

### Stream-JSON Output Processing

During subagent execution, output is streamed as JSON events:
- `init`: Agent initialization
- `message`: Including `approval_request_message` for permission handling
- `auto_approval`: Automatic permission grants
- `result`: Final output
- `error`: Error handling

### Key Patterns

- **CLI-based spawning** (like zdx!): Each subagent is a child CLI process
- **Stream-JSON for live updates**: Rich real-time feedback during subagent execution
- **Dynamic tool description injection**: Model discovers available agents through the Task tool description
- **React-based TUI display**: `SubagentGroupDisplay` component shows parallel agent status

---

## Cross-System Comparison (All 5 Systems)

| Aspect | Claude Code | Kimi K2.5 | OpenAI Codex | Letta Code | zdx (current) |
|--------|------------|-----------|-------------|-----------|--------------|
| **Execution** | Local CLI processes | Server-side inference | Thread | CLI process | CLI process |
| **Max agents** | ~3-5 typical | Up to 100 | — | — | 1 at a time |
| **Parallelism** | Yes (separate processes) | Yes (concurrent API calls) | Yes (threads) | Yes (spawns) | Yes (`tokio::JoinSet`) |
| **Inter-agent messaging** | Yes (SendMessage) | No (results only) | Yes (send_prompt) | No (stream events) | No |
| **Shared task list** | Yes (file-based JSON) | No | No | No | No |
| **Model training** | None (prompt-only) | PARL (RL) | None | None | None |
| **File access** | Full local filesystem | Container-only (cloud) | Full local | Full local | Full local |
| **Agent identity** | Name + team config | Custom system_prompt | AgentRole enum | SubagentConfig | `invoke_subagent` + optional `model` |
| **Coordination state** | `~/.claude/teams/` + tasks | Orchestrator context only | AgentControl threads | Stream-JSON events | None (fire-and-forget) |
| **Lifecycle** | spawn→work→idle→message→shutdown→cleanup | create→assign→result | spawn→work→result | spawn→stream→result | spawn→wait→result |
| **Shutdown** | Graceful protocol (request→approve→cleanup) | Automatic | Automatic | Automatic | Automatic |
| **Resume** | Yes (agent ID) | No | Yes (rollout files) | No | No |
| **Background** | Yes (output file) | N/A (server-side) | N/A | N/A | No |
| **Depth limit** | Subagents can't spawn subagents | N/A | MAX_THREAD_SPAWN_DEPTH | N/A | N/A |
| **Open source** | Prompts extracted, code proprietary | Model+paper open, infra proprietary | Fully open | Fully open | Fully open |

## Cross-System Agent Profile Comparison

| Role | Claude Code | OpenAI Codex | Letta Code | Notes |
|------|------------|-------------|-----------|-------|
| **Fast search** | Explore: Haiku, read-only, fresh context | Explorer: `codex-mini`, medium reasoning | explore: config-based | Universal: cheap model, no write access |
| **Full executor** | General-purpose: inherits model, all tools | Worker: fixed model override | (custom configs) | The "do anything" agent |
| **Planner** | Plan: inherits model, read-only, inherits context | Orchestrator: coordination-only | — | Read-only research for planning |
| **Coordinator** | Agent Teams lead (delegate mode: only team tools) | Orchestrator: "do NOT perform actual work" | — | Delegates, doesn't execute |
| **Docs/Guide** | Claude Code Guide: Haiku, WebFetch+WebSearch+Read | — | — | Answers questions about the tool itself |
| **Code reviewer** | — (custom agent) | — | code-reviewer: config-based | Common custom profile |

---

## Design Patterns & Lessons

### Pattern 1: Context Sharding (Kimi)
Each sub-agent gets a focused context. Returns summary, not full trace. Orchestrator stays clean. Effectively multiplies usable context window.

### Pattern 2: Proactive Task Management (Claude Code)
Task list serves dual purposes: user visibility (activeForm spinner) and agent memory (prevents forgetting tasks). Teammates can see what's available, claimed, blocked.

### Pattern 3: Learned Parallelism (Kimi PARL)
Model decides when to parallelize vs serialize. Without RL training, prompt hint "launch multiple agents concurrently whenever possible" is the next best thing.

### Pattern 4: Fire-and-Forget vs Collaborative
- **Fire-and-forget** (Kimi, zdx): assign task → get result. Simple, no mid-task correction.
- **Collaborative** (Claude Code): agents message each other, share findings, challenge ideas. Complex but richer.

### Pattern 5: Agent Specialization via System Prompt
Both Kimi and Claude Code create specialized agents by writing custom system prompts at runtime. Claude Code also supports user-defined agent `.md` files with YAML frontmatter.

### Pattern 6: Background Agents (Claude Code)
Agents can run in background with output written to a file. Main agent continues working, can check progress by reading the output file, can resume agents later by ID.

### Pattern 7: Graceful Lifecycle Management (Claude Code)
Full lifecycle: spawn → work → idle → message → shutdown (request→approve→cleanup).

### Pattern 8: Step Budgets (Kimi)
Explicit tool-call budgets per agent. Different budgets for orchestrator (lower, just coordinates) vs sub-agents (higher, do actual work). Prevents runaway execution.

### Pattern 9: Dynamic Description Injection (Letta)
Dynamically update tool description with available profiles at startup. Makes profiles discoverable through the tool schema itself.

### Pattern 10: Depth Limit (Codex)
`MAX_THREAD_SPAWN_DEPTH` prevents infinite recursion. Simple: pass depth counter via env var to child process.

---

## Ideas for zdx Subagent Improvements

### 1. Parallel Execution — ✅ ALREADY IMPLEMENTED

zdx already executes all tool calls from a single model turn concurrently via `tokio::JoinSet` in `execute_tools_async()`. If the model emits 3 `Subagent` tool calls in one response, they run in parallel automatically. Just need prompt hints.

### 2. Subagent Profiles (PRIMARY IMPROVEMENT) — Recommended

All competitors use pre-defined profiles rather than letting the model write system prompts inline. This is the single most impactful improvement.

**Current zdx approach:**
```json
{"name": "Subagent", "prompt": "find auth patterns", "system_prompt": "You are a read-only explorer...", "model": "..."}
```
The model must configure everything inline — more knobs = more mistakes.

**Proposed approach: profiles only, no inline system_prompt:**
```json
{"name": "Subagent", "prompt": "find auth patterns", "profile": "explore"}
```

**Why remove `system_prompt` from the tool schema:**
- Less opportunity for the model to be wrong
- Profiles are human-authored and tested — higher quality
- Simpler tool schema = model uses it more reliably
- All configuration power moves to profile definitions (user controls)

**Proposed zdx profiles:**

| Profile | Model | Tools | Purpose | Inspired By |
|---------|-------|-------|---------|-------------|
| `explore` | Cheap/fast (e.g., gemini-2.5-flash) | Read-only | Fast codebase search | Claude Code Explore, Codex Explorer |
| `worker` | Inherits parent | All tools | General-purpose task executor | Claude Code General-purpose, Codex Worker |
| `researcher` | Inherits parent | No tools (thinking only) | Research and analysis | Kimi sub-agents |

**Profile definition format:**
```toml
[profiles.explore]
description = "Fast read-only codebase search. Use for finding files, patterns, and understanding code."
model = "gemini:gemini-2.5-flash"  # cheap and fast
no_tools = false
# tool_filter = ["Read", "Grep", "Glob"]  # future: restrict tools

[profiles.worker]
description = "General-purpose task executor with full tool access."
# model = inherits parent

[profiles.researcher]
description = "Research and analysis agent. Returns concise summaries."
no_tools = true  # just thinking, no file access
```

**Simplified tool schema:**
```json
{
  "name": "Subagent",
  "description": "Delegate a scoped task to an isolated child agent. Choose a profile that matches the task. Launch multiple subagents concurrently by emitting multiple calls in a single response when tasks are independent.",
  "parameters": {
    "prompt": { "type": "string", "description": "Task instructions. Be specific about expected output." },
    "profile": { "type": "string", "description": "Agent profile: explore (fast read-only codebase search), worker (full capability), researcher (analysis)." }
  },
  "required": ["prompt", "profile"]
}
```

**Key design decisions:**
- `profile` required — forces the model to pick every time
- No `system_prompt` or `model` params — profiles own configuration
- Profile descriptions injected into tool schema (Letta pattern)
- User-extensible via config or `.zdx/profiles/`

### 3. Prompt Hints for Parallel Execution

Add hints from both Claude Code and Kimi's tool descriptions:
```
"Launch multiple subagents concurrently by emitting multiple Subagent tool calls
in a single response. Do this whenever tasks are independent and parallelizable."
```

### 4. Context Sharding Hint

Guidance that results should be summaries, not full traces (Kimi's insight):
```
"The subagent returns a concise summary of its work.
This preserves your context window for coordination."
```

### 5. Depth Limit (Codex pattern)

Prevent subagents from spawning subagents. Pass a depth counter via env var to child process, reject if exceeded.

### 6. Future: Background Execution (Claude Code pattern)

`run_in_background` flag. Subagent writes output to a temp file. Main agent can check progress and continue working. Deferred — only needed when latency becomes a pain point.

### 7. Future: Agent Resume (Claude Code / Codex pattern)

Resume a previous subagent session by ID. Deferred — only needed for long-running follow-up workflows.

### 8. Future: Dynamic Description Injection (Letta pattern)

Dynamically update the Subagent tool description with available profiles at startup. Makes profiles discoverable without hardcoding them in the tool schema string.

---

## Sources

- **Claude Code system prompts**: https://github.com/Piebald-AI/claude-code-system-prompts
- **Claude Code Agent Teams docs**: https://code.claude.com/docs/en/agent-teams
- **Claude Code Subagents docs**: https://code.claude.com/docs/en/sub-agents
- **Kimi K2.5 paper**: https://arxiv.org/abs/2602.02276
- **Kimi K2.5 tech blog**: https://kimi.com/blog/kimi-k2-5.html
- **Kimi leaked internals**: https://github.com/dnnyngyen/kimi-agent-internals
- **Kimi model weights**: https://huggingface.co/moonshotai/Kimi-K2.5
- **OpenAI Codex CLI**: https://github.com/openai/codex
- **Letta Code**: https://github.com/letta-ai/letta-code
- **Building a C compiler with agent teams**: https://www.anthropic.com/engineering/building-c-compiler
- **Towards AI comparison**: https://pub.towardsai.net/inside-claude-codes-agent-teams-and-kimi-k2-5-s-agent-swarm-0106f2467bd2