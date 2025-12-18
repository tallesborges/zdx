# ADR 0001: Session format: JSONL append-only
Date: 2025-12-18
Status: Accepted

## Context
ZDX needs durable, resumable sessions that are:
- easy to write incrementally while streaming
- robust to interruption (best-effort append)
- inspectable with standard CLI tools
- simple to evolve with explicit schema rules

Alternatives considered included a single JSON file per session, SQLite, and “no persistence”.

## Decision
Persist sessions as **JSONL (newline-delimited JSON)**, append-only, with a required first-line `meta` event containing a `schema_version`.

## Consequences
- Easy to append events as they happen (streaming-friendly).
- Corruption is typically localized to the last partial line.
- Human/debuggable with `cat`, `tail`, `jq`, and greppable logs.
- Requires explicit schema/versioning rules and backward-reading support.

## Alternatives Considered
- Single JSON document: harder to update safely during streaming; higher corruption blast radius.
- SQLite: more robust queries but heavier operational/implementation complexity for v0.x.
- No persistence: breaks “resume” and reduces day-to-day utility.

