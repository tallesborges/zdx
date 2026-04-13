# zdx-core development guide

Scope: compatibility facade — re-exports everything from `zdx-engine` so surface crates
(`zdx-cli`, `zdx-tui`, `zdx-bot`, `zdx-monitor`) can keep using `zdx_core::*` imports.

## Where things are

- `src/lib.rs`: re-exports all `zdx_engine` modules

All runtime code now lives in `crates/zdx-engine/`. See `crates/zdx-engine/AGENTS.md` for the full map.

## Conventions

- Do not add new code here. New modules go in `zdx-engine`.
- Keep this crate as a thin re-export layer only.

## Checks

- Default final verification after code changes: `just ci` from repo root
- Intermediate iteration: `cargo check -p zdx-core`

## Maintenance

- When a new module is added to `zdx-engine`, add a corresponding `pub use` in `src/lib.rs`.
- Architecture changes: update `docs/ARCHITECTURE.md`.
