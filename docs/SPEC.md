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
- `zdx exec -p, --prompt <PROMPT>` — run one prompt non-interactively
- `zdx automations list|validate|daemon|runs [NAME] [--date*] [--json]|run <NAME>`
- `zdx threads list|show <ID>|resume [ID]|search [QUERY] [--date*] [--limit N] [--json]`
- `zdx config init|path`

**Exit codes:** `0` success, `1` runtime error, `2` CLI usage error, `130` interrupted.

---

## 7) Output Contracts

### `zdx exec` (non-interactive, scriptable)

- **stdout:** assistant text only (or JSON if/when `--format json` ships).
- **stderr:** diagnostics, warnings, tool status, errors.

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

**Shipped:** Anthropic Claude (API key; streaming + tool loop), Claude CLI (Anthropic Messages API + OAuth), OpenAI Codex (Responses API + OAuth), OpenAI API (Responses + API key), OpenRouter (OpenAI-compatible chat completions + API key), Moonshot (Kimi API; OpenAI-compatible chat completions + API key), MiMo (Xiaomi MiMo; OpenAI-compatible chat completions + API key), Gemini (Generative Language API + API key), Gemini CLI (Cloud Code Assist + OAuth).

- API keys are env-only (never stored in config):
  - `ANTHROPIC_API_KEY`
  - `OPENAI_API_KEY`
  - `OPENROUTER_API_KEY`
  - `MOONSHOT_API_KEY`
  - `MIMO_API_KEY`
  - `GEMINI_API_KEY`
- Anthropic uses API key auth only.
- OAuth tokens may be cached in `<base>/oauth.json` (Claude CLI, OpenAI Codex, Gemini CLI).
- Claude CLI uses OAuth tokens from `<base>/oauth.json` (login via `zdx login --claude-cli`).
- OpenAI Codex uses OAuth tokens from `<base>/oauth.json` (login via `zdx login --openai-codex`).
- Gemini CLI uses OAuth tokens from `<base>/oauth.json` (login via `zdx login --gemini-cli`).
- Provider selection:
  - Explicit prefixes: `openai:`, `openrouter:`, `moonshot:`, `kimi:`, `mimo:`, `gemini:`, `gemini-cli:`, `google-gemini-cli:`, `anthropic:`, `claude-cli:`, `codex:` (also `openrouter/`).
  - Heuristics: models containing `codex` → OpenAI Codex; `gpt-*`/`o*` → OpenAI; `kimi-*`/`moonshot-*` → Moonshot; `mimo-*` → MiMo; `gemini-*` → Gemini; `claude-*` → Anthropic.

---

## 11) Configuration

- Location: `<base>/config.toml`
- Format: TOML
- Keys: `model`, `max_tokens`, `tool_timeout_secs`, `system_prompt`, `system_prompt_file`, `prompt_template.*`, `thinking_level`, `subagents.*`
  - `max_tokens` is optional; when unset, requests use the model output limit (exclusive, minus 1).
- Provider base URLs:
  - `[providers.anthropic].base_url`
  - `[providers.claude_cli].base_url`
  - `[providers.openai].base_url`
  - `[providers.openai_codex].base_url` (unused; reserved)
  - `[providers.openrouter].base_url`
  - `[providers.moonshot].base_url`
  - `[providers.mimo].base_url`
  - `[providers.gemini].base_url`
  - `[providers.gemini_cli].base_url` (unused; reserved)
- Provider tool configuration:
  - `[providers.<provider>].tools` — list of enabled tools
  - Available tools: `bash`, `apply_patch`, `edit`, `fetch_webpage`, `invoke_subagent`, `read`, `read_thread`, `thread_search`, `web_search`, `write`
  - Default tool sets:
    - Most providers: `["bash", "edit", "fetch_webpage", "invoke_subagent", "read", "read_thread", "thread_search", "web_search", "write"]`
    - OpenAI Codex: `["bash", "apply_patch", "fetch_webpage", "invoke_subagent", "read", "read_thread", "thread_search", "web_search"]`
- Models registry:
  - `[providers.<provider>]` (`enabled`, `models`)
  - `models` entries support `*` wildcards for `zdx models update`.
  - Registry path: `<base>/models.toml` (falls back to `default_models.toml` when missing).
- Skills:
  - `[skills.sources]` source flags (`zdx_user`, `zdx_project`, `codex_user`, `claude_user`, `claude_project`, `agents_user`, `agents_project`).
  - `skill_repositories`: list of GitHub repo paths (`owner/repo/path`) used by the skill installer overlay.
  - Optional glob filters: `ignored_skills`, `include_skills`.
- Subagents:
  - `[subagents].enabled` — enable/disable `invoke_subagent` tool exposure.
  - `[subagents].allowed_models` — allowed models for `invoke_subagent` (empty means any).
- Prompt templating:
  - `[prompt_template].file` — optional template file path (relative paths resolve from `ZDX_HOME`).
  - Template syntax uses MiniJinja (`{{ var }}`, `{% if %}`, `{% for %}`).
  - Render context includes: `agent_identity`, `provider`, `invocation_term`, `invocation_term_plural`, `is_openai_codex`, `base_prompt`, `project_context`, `memory_index`, `memory_suggestions`, `surface_rules`, `skills_list`, `subagents_config`, `cwd`, `date`.
  - Built-in template emits `<surface_rules>` only when `surface_rules` is present/non-empty.
  - On custom template load/render failure, ZDX warns and falls back to the built-in template.
  - Providers do not prepend hidden/provider-specific coding system prompts; they consume the caller-composed prompt.

---

## 12) Project Context + Memory (`AGENTS.md`, `MEMORY.md`)

ZDX composes project/user context in this order before skills/subagents sections:

1. Base/system prompt from config (`system_prompt` / `system_prompt_file`)
2. Hierarchical `AGENTS.md` context (global + user + project ancestry)
3. Optional memory index from `$ZDX_HOME/MEMORY.md`

Contracts:

- Memory is optional. Missing `MEMORY.md` does not fail startup and does not inject memory blocks.
- `MEMORY.md` load failures are warnings (non-fatal).
- `MEMORY.md` content is capped at 16 KiB with truncation warning.
- Only `MEMORY.md` index content is injected. Detailed memory lives in NotePlan and is accessed on-demand via the `noteplan-notes` skill.
- Built-in template emits a `## Memory` section (with `<memory>` block) only when memory index content is present.
- Proactive memory-save suggestion instructions are surface-gated: enabled for TUI and Telegram sessions, disabled for exec mode, automations, and subagent runs.
- Explicit `remember X` still means immediate save regardless of proactive suggestion mode.

---

## 13) Skills (SKILL.md)

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

## 14) Telegram bot media response (`zdx-bot`)

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
