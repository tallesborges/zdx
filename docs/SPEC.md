# ZDX Specification

**Product:** ZDX (TUI-first terminal assistant for developers)  
**Status:** Source of truth for *vision + user-visible contracts*. ADRs explain “why”; plans explain “how”.  
**Notation:** **Shipped** = present in this repo today. **Target (TUI2)** = intended full-screen TUI behavior.

---

## 1) Vision

ZDX is a **daily-driver terminal app** you can keep open all day: calm, fast, and dependable under real work.

The TUI is the product. A CLI mode exists to support automation and scripting.

---

## 2) Why

Terminal AI tools often break the parts that matter daily:
- flicker/jank from naive redraw
- resize bugs and “lost” history
- mixed stdout/stderr corrupting the screen
- weak transcript UX (scroll/select/copy)
- no durable history you can trust

ZDX solves this with a boring, reliable core:
- own the viewport (TUI)
- transcript as the source of truth
- UI-agnostic engine (events)
- deterministic tools
- append-only session log

---

## 3) Goals

### Primary: `zdx` (interactive)

- Full-screen terminal chat UI that stays stable under resizes, overlays, long sessions, and continuous streaming.
- Transcript UX: scroll, select, copy.
- Sessions persist and replay deterministically.

### Secondary: `zdx exec ...` (non-interactive)

- Script-friendly execution with clean stdout/stderr separation.
- Same engine, different renderer.

### Current focus (non-contract)

- TUI2 baseline: alt-screen + raw mode + clean restore
- Transcript cells + width-agnostic rendering + resize-safe reflow
- Smooth streaming (throttled redraw, no flicker)
- Scroll model: follow-latest vs anchored
- Selection + copy that matches what you see (incl. off-screen)

---

## 4) Non-goals

- Cooperating with terminal scrollback while the TUI is running.
- Guaranteeing stdout piping while the TUI is active (use `exec` for that).
- Terminal-dependent rendering tricks (scroll regions / partial clears) as a correctness mechanism.
- IDE ambitions (file tree, refactor UI, indexing) in early versions.
- Safety sandboxing as a primary product goal (YOLO default).

---

## 5) Principles

- **TUI-first UX:** optimize for reading/navigation/editing in a full-screen terminal app.
- **Own the viewport:** redraw from in-memory state; the terminal is a render target, not a data store.
- **Engine/UI separation:** engine emits events; renderers do terminal I/O.
- **KISS/YAGNI:** ship the smallest daily-driver value; refactor only after usage proves shape.
- **YOLO default:** prioritize speed/flow on the user’s machine; guardrails are opt-in and low friction.
- **User journey drives order:** build in the order the user experiences it: start → input → submit → see output → stream → scroll/navigate → follow-up interactions → polish.
- **Ship-first:** prioritize a daily-usable MVP early; refactor later (YAGNI).
- **Demoable slices:** every slice must be runnable and include ✅ Demo criteria.
- **For UI work:** prefer reducer pattern: update(state, event); render reads state only.
- **Call out key decisions/risks early:** (keybindings + focus, input vs navigation conflicts, backpressure, performance).

---

## 6) Product surface (CLI)

**Shipped commands (v0.1):**
- `zdx` — interactive chat (TTY)
- `zdx exec -p, --prompt <PROMPT>` — run one prompt non-interactively
- `zdx sessions list|show <ID>|resume [ID]`
- `zdx config init|path`

**Planned (not shipped):**
- `zdx chat` — explicit interactive entrypoint (alias of `zdx`)

Exit codes (v0.1): `0` success, `1` runtime error, `2` CLI usage error, `130` interrupted.

---

## 7) Output channel contracts

### `zdx exec` (non-interactive, scriptable)

- **stdout:** assistant text only (or JSON if/when `--format json` ships).
- **stderr:** diagnostics, warnings, tool status, errors.

### `zdx` (interactive)

- **Shipped (v0.1):** transcript streams to stdout; editor/tool status use stderr.
- **Target (TUI2):** full-screen alt-screen TUI; **does not print transcript to stdout while active**.
  - Any diagnostics should be shown in the UI; optional file logging is acceptable.

---

## 8) Architecture contract (engine + renderers)

Engine emits an event stream consumed by a renderer (CLI or TUI). See ADR-0002 and ADR-0003.

**Hard rule:** engine performs no terminal I/O (`println!`, styling, cursor moves). Renderers own stdout/stderr and raw mode.

### Required engine event types (Shipped)

- `AssistantDelta { text }`, `AssistantFinal { text }`
- `ToolRequested { id, name, input }`, `ToolStarted { id, name }`, `ToolFinished { id, result }`
- `Error { kind, message, details? }`
- `Interrupted`

---

## 9) Transcript model (Target: TUI2)

The transcript is the source of truth and is **width-agnostic**:

```text
Vec<HistoryCell>
```

Each cell is a logical unit: user block, assistant block (streaming/final), tool block, system/info banner.

### Rendering contract

- Each cell can render display lines for a given width: `display_lines(width) -> Vec<StyledLine>`.
- Wrapping happens at display time for the current width.
- Per draw: flatten rendered lines → apply scroll → render visible slice → apply selection overlay.

### Scroll / selection / copy (Target: TUI2)

- Scrolling operates on flattened visual lines (not terminal scrollback).
- Two scroll states: `FollowLatest` and `Anchored`.
- Selection is defined over the flattened transcript (line/col), excluding any left gutter/prefix.
- Copy reconstructs text using the same wrapping rules (including code block indentation and emoji/wide glyph widths).

### Streaming (Target: TUI2)

- Store raw markdown (append-only) + a logical “commit cursor”.
- Render by parsing markdown → styling spans → wrapping at current width → revealing committed prefix.
- Throttle redraw during streaming; avoid flicker and whole-transcript rewrap per frame.

---

## 10) Sessions (persistence contract)

Sessions are append-only **JSONL** event logs (ADR-0001).

### Storage

- Base dir: `$ZDX_HOME` (preferred) else `$XDG_CONFIG_HOME/zdx` else `~/.config/zdx`
- Sessions dir: `<base>/sessions/`
- OAuth cache: `<base>/oauth.json` (0600 perms)

### Format (Shipped)

- First line is `meta` with `schema_version`.
- Timestamps are RFC3339 UTC.
- Minimum event set includes: `meta`, `message`, `tool_use`, `tool_result`, `interrupted`.
- Sessions remain readable even if interrupted mid-stream.

---

## 11) Tools (deterministic contract)

Tools are intentionally few, stable, and machine-parseable.

### Envelope (Shipped)

Success:
```json
{ "ok": true, "data": { ... } }
```

Error:
```json
{ "ok": false, "error": { "code": "...", "message": "..." } }
```

### Semantics (Shipped)

- Tool results are deterministic and correspond to the correct `tool_use_id`.
- Relative paths resolve against `--root` (default `.`).
- `--root` is a working directory context, not a security boundary (YOLO).

---

## 12) Providers (Shipped + planned)

**Shipped:** Anthropic Claude (streaming + tool loop).  
API keys are env-only and never stored in config (`ANTHROPIC_API_KEY`).
OAuth tokens (Anthropic account login) may be cached in `<base>/oauth.json`.
Auth precedence: `oauth.json` > `ANTHROPIC_API_KEY`.

**Testability requirement (Shipped):** provider calls support a base URL override for offline fixture tests.

---

## 13) Configuration (Shipped)

- Location: `<base>/config.toml` (see §10 storage rules)
- Format: TOML
- Keys: `model`, `max_tokens`, `tool_timeout_secs`, `system_prompt`, `system_prompt_file`, `anthropic_base_url`

---

## 14) Tests (minimum bar)

Tests protect contracts, not internals.

- Tool loop sequencing (`tool_use` ↔ `tool_result`) and offline provider fixtures
- Session JSONL read/write resilience (incl. interrupts)
- Target (TUI2): wrapping + resize reflow, scroll model, selection/copy fidelity, streaming invariants

---

## 15) Project context (AGENTS.md) (Shipped)

ZDX loads `AGENTS.md` hierarchically and appends the content to the system prompt (project-specific guidance). Unreadable files warn; empty files are skipped.
