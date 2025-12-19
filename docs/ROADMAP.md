# Roadmap (optional)

> **Principle:** Build the engine once, then render it in CLI now and TUI later.
>
> **Note:** ROADMAP describes outcomes (what). `docs/SPEC.md` is the source of truth for contracts, and `docs/adr/` captures decision rationale (why).
>
> **Maintenance rule:** Prefer **Now / Next / Later** over version micro-buckets; assign versions only when cutting a release.

---

## Shipped

- Durable JSONL sessions and streaming CLI output
- Commands: `exec`, interactive chat mode, `sessions list/show`, `resume`, `config init/path`
- CLI options: `--root`, `--system-prompt`, `--session`, `--no-save`
- Provider: Anthropic Claude (streaming)
- Tools: `read`, `bash` with tool loop + stderr tool indicators
- `AGENTS.md` hierarchical auto-inclusion (and surfaced to stderr)

---

## Now (max 3)

- Make engine/renderer separation strict and boring (engine emits events; renderer owns I/O)
- Provider testability without network (base URL override + fixtures for streaming/tool loop)
- Terminal UX polish (consistent stderr errors; readable transcripts; pipe-friendly output)

---

## Next

- Add minimal authoring tools (`write`, then `edit`) with deterministic results and clear failures
- TUI MVP powered by the same engine event stream (no forked logic)
- Context attachments that stay explicit and predictable (e.g., `--file <path>`)

---

## Later

- Script-friendly JSON output stabilization for `exec --format json` (schema-versioned contract)
- Session inspection: search/filter/export + offline viewer
- Optional mutation visibility (diff preview) and opt-in confirmations (still YOLO default)
- Optional additional providers behind the same provider contract
- [Agent Skills](https://agentskills.io/specification) support: load `SKILL.md` files with progressive disclosure (metadata → instructions → resources)
