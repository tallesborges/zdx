# Architecture Decision Records (ADRs)

ADRs capture the *why* behind notable decisions over time.

## Rules

- One decision per file.
- Don’t rewrite history: supersede with a new ADR instead of editing old decisions.
- Keep ADRs small and specific; focus on context → decision → consequences.

## Naming

- `NNNN-slug.md` (4 digits, then a short kebab-case slug)
- Reference format: `ADR-0001`

## Template

Copy/paste from `docs/adr/0000-template.md`.

## Index

- [ADR-0001: Session format: JSONL append-only](./0001-session-format-jsonl.md)
- [ADR-0002: Agent emits events to a renderer sink](./0002-agent-emits-events-to-renderer-sink.md)

