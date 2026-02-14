# xtask development guide

Scope: maintainer-only workspace utilities (defaults/config/codebase generation).

## Where things are

- `src/main.rs`: xtask command entrypoint

## Conventions

- Keep xtask focused on repository maintenance workflows.
- Avoid runtime dependencies from product crates unless required.

## Checks

- Targeted: `cargo test -p xtask`
- Run commands from repo root via `cargo xtask ...` or `just update-*` recipes

## Maintenance

- Add/move/delete `.rs` files in this crate: update this file.