# ZDX Specification

**Product:** ZDX (TUI-first terminal assistant for developers)  
**Status:** Source of truth for *vision + user-visible contracts*.

---

## 1) Vision

ZDX is a **daily-driver terminal app** you can keep open all day: calm, fast, and focused on developer productivity.

The TUI is the product. A CLI mode exists to support automation and scripting.

---

## 2) Why

**Learning by building.** This project exists to explore how agentic coding tools work by implementing one from scratch.

**Developer UX is the priority.** Every feature should reduce friction and help developers move faster. If it doesn't improve the daily workflow, it doesn't belong.

Terminal AI tools often break the parts that matter daily:
- flicker/jank from naive redraw
- resize bugs and "lost" history
- mixed stdout/stderr corrupting the screen
- weak transcript UX (scroll/select/copy)
- no durable history you can trust

ZDX solves this with a boring, reliable core:
- own the viewport (TUI)
- transcript as the source of truth
- UI-agnostic agent (events)
- deterministic tools
- append-only thread log

---

## 3) Goals

### Primary: `zdx` (interactive)

- Full-screen terminal chat UI that stays stable under resizes, overlays, long threads, and continuous streaming.
- Transcript UX: scroll, select, copy.
- Threads persist and replay deterministically.
- **Queued prompts:** when a turn is streaming, submitting a normal prompt enqueues it. The next queued prompt auto-sends when the turn ends. A small queue panel appears between transcript and input (first 3 prompts, 30-char summaries). Queue is in-memory only.

### Secondary: `zdx exec ...` (non-interactive)

- Script-friendly execution with clean stdout/stderr separation.
- Same agent, different renderer.

---

## 4) Non-goals

- Cooperating with terminal scrollback while the TUI is running.
- Guaranteeing stdout piping while the TUI is active (use `exec` for that).
- Terminal-dependent rendering tricks (scroll regions / partial clears) as a correctness mechanism.
- IDE ambitions (file tree, refactor UI, indexing) in early versions.
- Safety sandboxing as a primary product goal (YOLO default).

---

## 5) Principles

- **Developer UX is the priority:** features that improve daily workflow win. Ship fast unless it degrades core UX.
- **TUI-first UX:** optimize for reading/navigation/editing in a full-screen terminal app.
- **KISS/YAGNI:** ship the smallest daily-driver value; refactor only after usage proves shape.
- **Ship-first:** get it working, ship it, learn from usage. Refactor when the shape is proven.
- **User journey drives order:** build in the order the user experiences it: start → input → submit → see output → stream → scroll/navigate → follow-up interactions → polish.
- **Learn by doing:** explore TUI tech hands-on; accept messy code as part of the learning process.
- **YOLO default:** no guardrails, prioritize speed and flow.

---

## 6) Product Surface (CLI)

**Shipped commands (v0.1):**
- `zdx` — interactive chat (TTY)
- `zdx exec -p, --prompt <PROMPT> [--no-system-prompt]` — run one prompt non-interactively
- `zdx imagine -p, --prompt <PROMPT> [--out PATH] [--model MODEL] [--aspect RATIO] [--size SIZE]` — generate images with Gemini image models
- `zdx automations list|validate|daemon|runs [NAME] [--date*] [--json]|run <NAME>`
- `zdx threads list|show <ID>|resume [ID]|search [QUERY] [--date*] [--limit N] [--json]`
- `zdx config init|path`

**Exit codes:** `0` success, `1` runtime error, `2` CLI usage error, `130` interrupted.

---

## 7) Output Contracts

### `zdx exec` (non-interactive, scriptable)

- **stdout:** assistant text only (or JSON if/when `--format json` ships).
- **stderr:** diagnostics, warnings, tool status, errors.
- `--no-system-prompt` disables all system/context composition for that run (config system prompt, `AGENTS.md`, memory, skills).

### `zdx imagine` (non-interactive, scriptable)

- **stdout:** generated image file path(s), one per line.
- **stderr:** diagnostics and errors.

### `zdx` (interactive)

- Full-screen alt-screen TUI; **does not print transcript to stdout while active**.
- Any diagnostics are shown in the UI; optional file logging is acceptable.

---

## 8) Threads

Threads are append-only **JSONL** event logs (thread events are never modified or deleted).

### Storage

- Base dir: `$ZDX_HOME` (if set) else `~/.zdx`
- Threads dir: `<base>/threads/`
- OAuth cache: `<base>/oauth.json` (0600 perms)

### Format

- First line is `meta` with `schema_version` and optional `title`.
- Timestamps are RFC3339 UTC.
- Event types: `meta`, `message`, `tool_use`, `tool_result`, `interrupted`, `reasoning`.
- Threads remain readable even if interrupted mid-stream.

### Metadata Updates

The `meta` line (first line only) may be rewritten atomically to update thread metadata (e.g., `title`). This uses write-to-temp-then-rename for safety. Thread events after the meta line are never modified.

### Automation sessions

- Manual and daemon runs persist to timestamped thread IDs by default: `automation-<name>-<YYYYMMDD-HHMM>`.
- `zdx automations run <name> --thread <ID>` uses the explicit thread ID instead.
- `--no-thread` disables persistence for that run.
- Automation frontmatter may include `subagent: <name>` to run with a named subagent prompt/tool/model configuration.

---

## 9) Tools

Tools are intentionally few, stable, and machine-parseable.

### Envelope

Success:
```json
{ "ok": true, "data": { ... } }
```

Error:
```json
{ "ok": false, "error": { "code": "...", "message": "..." } }
```

### Semantics

- Tool results are deterministic and correspond to the correct `tool_use_id`.
- Relative paths resolve against `--root` (default `.`).
- `--root` is a working directory context, not a security boundary (YOLO).

---

## 10) Providers

Providers are the bridge between the agent and LLM APIs. New providers can be added without updating this spec as long as they follow these contracts.

### Auth contracts

- **API-key providers:** keys come from environment variables (`<PROVIDER>_API_KEY`), never stored in config.
- **OAuth providers:** tokens are cached in `<base>/oauth.json` (0600 perms). Login via `zdx login --<provider-slug>`.

### Model routing

- **Explicit prefix** (canonical): `<provider>:<model>` (e.g., `anthropic:claude-sonnet-4-5`). Always wins.
- **Heuristic fallback:** when no prefix is given, the model name is matched against provider-specific patterns (e.g., `claude-*` → Anthropic). Heuristics are implementation details and may change.

### Provider-level config

- Each provider may expose `base_url` and `tools` overrides under `[providers.<id>]` in config.
- Provider implementations live in `zdx-core`; the models registry (`models.toml`) tracks available models per provider.

---

## 11) Environment Variables (Runtime Context)

ZDX exposes runtime context to agent processes via `ZDX_*` environment variables.
These are the canonical source of truth for paths and session context — skills, automations, and bash commands reference these env vars directly.

### Mechanism

`set_runtime_env()` in `zdx-core/src/core/context.rs` sets all `ZDX_*` env vars once at agent startup (TUI, exec, bot). Child processes (bash tool, subagents) inherit them automatically.

### System prompt `<environment>` block

The `<environment>` block in the system prompt contains only non-path metadata that the model needs for reasoning without running commands (current directory, date). Paths are not duplicated in the prompt — the model uses `$ZDX_*` env vars directly in bash commands.

### Subagent inheritance

Child `zdx exec` processes inherit all `ZDX_*` env vars from the parent automatically. No explicit forwarding needed.

---

## 12) Configuration

- Location: `<base>/config.toml`
- Format: TOML

### Contracts

- Config is the single source of truth for user preferences (model, tokens, timeouts, prompt customization, memory paths, skill sources, subagent settings).
- Adding a new config key or provider section should not require a spec update — the config struct in code (`zdx-core`) is authoritative for the full schema.
- `max_tokens` is optional; when unset, providers that support omitted limits use provider defaults. Providers that require a limit use an internal fallback from model metadata.
- Provider base URLs and tool overrides live under `[providers.<id>]`.

### Prompt templating

- Template syntax: MiniJinja (`{{ var }}`, `{% if %}`, `{% for %}`).
- `[prompt_template].file` — optional template path (relative paths resolve from `ZDX_HOME`).
- The built-in fallback/default prompt is `prompts/system_prompt_template.md`. On custom template load/render failure, ZDX warns and falls back to that built-in template.
- Providers consume the caller-composed prompt; they do not prepend hidden coding system prompts.

### Prompt layers

- Prompt layers are additive MiniJinja-rendered prompt fragments appended after the base system prompt.
- The same mechanism is used for surface-specific constraints (for example Telegram or exec output guidance) and harness-style behavior layers (for example automation/headless execution instructions).
- Prompt layers modify behavior without creating a separate subagent identity.

### Named subagents

- Named subagents are markdown files with YAML frontmatter plus a standalone prompt body.
- Discovery order/override precedence: built-in → `~/.zdx/subagents/` → project `.zdx/subagents/` (later sources override earlier by name).
- `invoke_subagent` accepts `subagent: <name>`. When omitted, it uses the default/base system prompt behavior.
- When a named subagent is selected, its body becomes the child run's system prompt directly; it does not inherit the default ZDX prompt/context pipeline unless that text is written into the subagent body.
- Built-in subagents currently include `general_assistant` as a minimal general-purpose standalone subagent.

### Models registry

- Path: `<base>/models.toml` (falls back to `default_models.toml` when missing).
- Tracks available models per provider. Entries support `*` wildcards for `zdx models update`.

---

## 13) Project Context + Memory (`AGENTS.md`, `MEMORY.md`)

ZDX composes project/user context in this order before skills/subagents sections:

1. Base/system prompt from config (`system_prompt` / `system_prompt_file`)
2. Hierarchical `AGENTS.md` context (global + user + project ancestry)
3. Optional memory index from configured path (default: `$ZDX_HOME/MEMORY.md`)

### Memory configuration

```toml
[memory]
# notes_path = "~/SecondBrain/Notes"    # default: $ZDX_HOME/memory/notes
# daily_path = "~/SecondBrain/Calendar"  # default: $ZDX_HOME/memory/calendar
# index_file = "~/.zdx/MEMORY.md"       # default: $ZDX_HOME/MEMORY.md
```

- All paths support `~` expansion.
- Defaults place memory under `$ZDX_HOME/memory/` (notes + calendar subdirs) with the index at `$ZDX_HOME/MEMORY.md`.
- The `memory` skill provides full guidelines for working with memory notes (NotePlan-compatible conventions).

### Contracts

- Memory is optional. Missing `MEMORY.md` does not fail startup and does not inject memory blocks.
- `MEMORY.md` load failures are warnings (non-fatal).
- `MEMORY.md` content is capped at 16 KiB with truncation warning.
- Only `MEMORY.md` index content is injected. Detailed memory lives in the configured notes/daily paths and is accessed on-demand via the `memory` skill.
- Built-in template emits a `## Memory` section (with `<memory>` block and configured paths) only when memory index content is present.
- Proactive memory-save suggestion instructions are surface-gated: enabled for TUI and Telegram sessions, disabled for exec mode, automations, and subagent runs.
- Explicit `remember X` still means immediate save regardless of proactive suggestion mode.
- When proactive suggestions are enabled, memory instructions are note-first: save full detail in memory notes, and only promote durable/reusable items into `MEMORY.md`.
- `MEMORY.md` entries should be concise routing pointers; updates should prefer upsert/merge over append-only duplication.

---

## 14) Skills (SKILL.md)

Skills are folders containing a `SKILL.md` file with YAML frontmatter (`name`, `description`) and Markdown instructions. At startup, only metadata is loaded. The model uses the `read` tool to load full instructions when a task matches a skill.

### Discovery & sources

- **Recursive sources:** `~/.zdx/skills/`, project `.zdx/skills/`, `~/.codex/skills/`, `~/.agents/skills/`, and project `.agents/skills/` are scanned recursively for `SKILL.md`.
- **Claude sources (one-level):** `~/.claude/skills/` and project `.claude/skills/` only scan `dir/*/SKILL.md`.
- **Priority:** zdx-user → zdx-project → codex-user → claude-user → claude-project → agents-user → agents-project (first wins on name collision).

### Validation & warnings

- **Name:** required, ≤64 chars, lowercase alphanumeric + hyphens, no leading/trailing/consecutive hyphens.
- **Description:** required, ≤1024 chars.
- **Directory match:** name should match parent directory; mismatch emits a warning but still loads.
- Invalid skills are skipped with warnings; startup never fails.

### Prompt integration

- Skill metadata is appended to the system prompt as an `<available_skills>` XML block.
- Each skill includes `name`, `description`, `path` (absolute), and `source`.

### Filtering

- `include_skills`: optional glob allowlist (empty = all).
- `ignored_skills`: optional glob blocklist (wins over include).

## Related Documentation

- `docs/ARCHITECTURE.md` — TUI implementation patterns, code organization
- `docs/adr/` — Architecture Decision Records (the "why" behind decisions)
- `AGENTS.md` — Development guide and conventions

---

## 15) Telegram bot media response (`zdx-bot`)

When using the Telegram bot runtime, assistant turns may include media file directives.

Contracts:

- Text replies continue to work unchanged.
- If assistant output contains explicit media directives, the bot may send media in addition to text.
  - Supported entry formats:
    - `<media>/absolute/path/to/file</media>`
    - `<medias><media>/absolute/path/to/file1</media><media>/absolute/path/to/file2</media></medias>`
- Any `<media>` directives are stripped from the user-visible reply text.
- Routing by file type:
  - Image-like extensions (`.png`, `.jpg`, `.jpeg`, `.webp`) are sent via Telegram `sendPhoto`.
  - Other files (including `.pdf`) are sent via Telegram `sendDocument`.
- When multiple valid media paths are present, the bot attempts to send each one in order.
- Bot only uses local absolute file paths for this flow (no URL fetch in this slice).
- Preflight upload size checks:
  - photos > 10 MB are rejected before upload
  - documents > 50 MB are rejected before upload

---

## 16) Telegram forum topic flow (`zdx-bot`)

When the Telegram bot is used in a forum-enabled supergroup:

- A normal user message sent in `General` creates a new topic and routes that message into the topic before the agent replies.
- Slash commands that act on setup/status do not auto-create topics from `General`; they run in place instead (for example `/model`, `/thinking`, `/status`, `/worktree`).
- `/new` sent in `General` creates an empty topic only:
  - no prompt is routed into the new topic
  - no agent turn starts
  - no bot message is posted into the topic as part of creation
- Topic title generation rules:
  - if the topic was created from a normal message in `General`, the bot may auto-generate the topic title from that first routed message
  - if the topic was created by `/new` in `General`, the bot waits and auto-generates the topic title from the first later in-topic message that contains usable text (plain text or audio transcript)
