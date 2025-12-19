# Roadmap

> **Principle:** Build the engine once, then render it in CLI now and TUI later.
>
> **Note:** This file is optional. If present, ROADMAP describes outcomes (what). `docs/SPEC.md` is the source of truth for contracts, and `docs/adr/` captures decision rationale (why).
>
> **Maintenance rule:** Prefer **Now / Next / Later** over version micro-buckets; assign versions only when cutting a release.

---

## Shipped

- Durable JSONL sessions and streaming CLI output
- Commands: default interactive chat, `exec`, `sessions list/show/resume`, `config init/path`
- CLI options: `--root`, `--system-prompt`, `--session`, `--no-save`
- Provider: Anthropic Claude (streaming)
- Tools: `read`, `bash` with tool loop + stderr tool indicators
- `AGENTS.md` hierarchical auto-inclusion (and surfaced to stderr)

---

## Now (max 3)

- Engine has no direct terminal I/O (renderer owns stdout/stderr)
- Provider parsing/tool-loop tests run offline (base URL override + fixtures)
- Terminal UX contract is consistent (stdout-only assistant text; stderr-only UI/tools/errors)

---

## Next

- Add minimal authoring tools (`write`, then `edit`) with deterministic results and clear failures
- TUI MVP powered by the same engine event stream (no forked logic)
- Context attachments that stay explicit and predictable (e.g., `--file <path>`)
- Multi-provider support: OpenAI + Gemini (streaming + tool loop)
- Model switching UX (`--model` flag + config default)
- Login for subscription-based access (OpenAI / Gemini / Claude) with tokens stored in OS credential store (not config)
- Goal-based handoff bundles (Amp-style): generate a draft starter prompt + relevant file list to start a new focused session without lossy compaction

---

## Later

- Script-friendly JSON output stabilization for `exec --format json` (schema-versioned contract)
- Session inspection: search/filter/export + offline viewer
- Optional mutation visibility (diff preview) and opt-in confirmations (still YOLO default)
- OpenRouter provider support (OpenAI-compatible API)
- [Agent Skills](https://agentskills.io/specification) support: load `SKILL.md` files with progressive disclosure (metadata → instructions → resources)
