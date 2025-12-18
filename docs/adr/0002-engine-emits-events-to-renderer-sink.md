# ADR 0002: Engine emits events to a renderer sink
Date: 2025-12-18
Status: Accepted

## Context
ZDX aims to be terminal-first now and TUI-ready later without forking logic. That requires:
- a UI-agnostic engine (no printing/formatting)
- a stable event stream contract for renderers (CLI now, TUI later)
- clean stdout/stderr ownership at the renderer boundary

Alternatives included embedding rendering inside the engine, or building two parallel pipelines (CLI-specific vs TUI-specific).

## Decision
The engine emits a stream of `EngineEvent` values into a renderer-provided sink/callback. Renderers are responsible for formatting, stdout/stderr routing, and UX presentation.

## Consequences
- One engine can power multiple renderers (CLI/TUI) without duplicating the agent/tool loop.
- Testing becomes simpler: assertions can be written against the event stream.
- Renderer complexity increases (it owns UX rules), but that’s aligned with terminal-first requirements.
- The `EngineEvent` contract becomes a versioned/stable surface and must evolve additively when possible.

## Alternatives Considered
- Engine prints directly: fast to ship but hard to reuse for TUI and breaks separation guarantees.
- Two pipelines: duplicates behavior and causes drift across UIs.

