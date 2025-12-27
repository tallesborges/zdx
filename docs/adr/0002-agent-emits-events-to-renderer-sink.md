# ADR 0002: Agent emits events to a renderer sink
Date: 2025-12-18
Status: Accepted

## Context
ZDX aims to be terminal-first now and TUI-ready later without forking logic. That requires:
- a UI-agnostic agent (no printing/formatting)
- a stable event stream contract for renderers (CLI now, TUI later)
- clean stdout/stderr ownership at the renderer boundary

Alternatives included embedding rendering inside the agent, or building two parallel pipelines (CLI-specific vs TUI-specific).

## Decision
The agent emits a stream of `AgentEvent` values into a renderer-provided sink/callback. Renderers are responsible for formatting, stdout/stderr routing, and UX presentation.

## Consequences
- One agent can power multiple renderers (CLI/TUI) without duplicating the agent/tool loop.
- Testing becomes simpler: assertions can be written against the event stream.
- Renderer complexity increases (it owns UX rules), but that’s aligned with terminal-first requirements.
- The `AgentEvent` contract becomes a versioned/stable surface and must evolve additively when possible.

## Alternatives Considered
- Agent prints directly: fast to ship but hard to reuse for TUI and breaks separation guarantees.
- Two pipelines: duplicates behavior and causes drift across UIs.

