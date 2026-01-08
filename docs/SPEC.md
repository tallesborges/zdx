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
- `zdx threads list|show <ID>|resume [ID]`
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

- Base dir: `$ZDX_HOME` (preferred) else `$XDG_CONFIG_HOME/zdx` else `~/.config/zdx`
- Threads dir: `<base>/threads/`
- OAuth cache: `<base>/oauth.json` (0600 perms)

### Format

- First line is `meta` with `schema_version` and optional `title`.
- Timestamps are RFC3339 UTC.
- Event types: `meta`, `message`, `tool_use`, `tool_result`, `interrupted`, `thinking`.
- Threads remain readable even if interrupted mid-stream.

### Metadata Updates

The `meta` line (first line only) may be rewritten atomically to update thread metadata (e.g., `title`). This uses write-to-temp-then-rename for safety. Thread events after the meta line are never modified.

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

**Shipped:** Anthropic Claude (streaming + tool loop), OpenAI Codex (Responses API + OAuth), OpenAI API (Responses + API key), OpenRouter (OpenAI-compatible chat completions + API key), Gemini (Generative Language API + API key).

- API keys are env-only (never stored in config):
  - `ANTHROPIC_API_KEY`
  - `OPENAI_API_KEY`
  - `OPENROUTER_API_KEY`
  - `GEMINI_API_KEY`
- OAuth tokens may be cached in `<base>/oauth.json`.
- Auth precedence for Anthropic: `oauth.json` > `ANTHROPIC_API_KEY`.
- OpenAI Codex uses OAuth tokens from `<base>/oauth.json` (login via `zdx login --openai-codex`).
- Provider selection:
  - Explicit prefixes: `openai:`, `openrouter:`, `gemini:`, `anthropic:`, `codex:` (also `openrouter/`).
  - Heuristics: models containing `codex` → OpenAI Codex; `gpt-*`/`o*` → OpenAI; `gemini-*` → Gemini; `claude-*` → Anthropic.

---

## 11) Configuration

- Location: `<base>/config.toml`
- Format: TOML
- Keys: `model`, `max_tokens`, `tool_timeout_secs`, `system_prompt`, `system_prompt_file`, `thinking_level`
- Provider base URLs:
  - `[providers.anthropic].base_url`
  - `[providers.openai].base_url`
  - `[providers.openai_codex].base_url` (unused; reserved)
  - `[providers.openrouter].base_url`
  - `[providers.gemini].base_url`
- Models registry:
  - `[providers.<provider>]` (`enabled`, `models`)
  - `models` entries support `*` wildcards for `zdx models update`.
  - Registry path: `<base>/models.toml` (falls back to `default_models.toml` when missing).

---

## 12) Project Context (AGENTS.md)

ZDX loads `AGENTS.md` hierarchically and appends the content to the system prompt (project-specific guidance). Unreadable files warn; empty files are skipped.

---

## Related Documentation

- `docs/ARCHITECTURE.md` — TUI implementation patterns, code organization
- `docs/adr/` — Architecture Decision Records (the "why" behind decisions)
- `AGENTS.md` — Development guide and conventions
