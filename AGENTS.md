# Repository Guidelines

## Project Structure & Module Organization

- `src/main.rs`: CLI entrypoint; wires subcommands to implementations.
- `src/cli.rs`: `clap` command/flag definitions (`Cli`, `Commands`, etc.).
- `src/config.rs`: config loading + initialization (`config.toml`), with unit tests.
- `src/paths.rs`: resolves `ZDX_HOME`/default paths (config + sessions).
- `tests/`: integration tests for CLI behavior (`assert_cmd`, `predicates`).

## Build, Test, and Development Commands

- `cargo build`: compile a debug build.
- `cargo build --release`: compile an optimized binary.
- `cargo run -- --help`: run the CLI (args after `--`).
- `cargo test`: run unit + integration tests.
- `cargo fmt`: format the codebase with Rustfmt.
- `cargo clippy`: lint the codebase (optionally add `-- -D warnings` for strict CI-like checks).

Example: `cargo run -- config init` (creates a default config file).

## Coding Style & Naming Conventions

- Rust edition: 2024 (see `Cargo.toml`).
- Formatting: Rustfmt defaults; 4-space indentation (standard Rust style).
- Naming: modules/files `snake_case.rs`, types `UpperCamelCase`, fns/vars `snake_case`.
- Errors: prefer `anyhow::Result` + `Context` at I/O boundaries for actionable messages.

## Testing Guidelines

- Unit tests live next to code (e.g., `src/config.rs`); integration tests in `tests/*.rs`.
- Prefer black-box CLI tests using `assert_cmd::cargo::cargo_bin_cmd!`.
- Naming: `test_<behavior>_<expected>()` and keep tests independent/isolated.

Run a single integration test file: `cargo test --test config_path`.

## Commit & Pull Request Guidelines

- Commit messages generally follow Conventional Commits (e.g., `feat: ...`, `fix: ...`).
- PRs should include: a clear description, repro steps (or example commands), and tests.
- Before opening a PR: run `cargo fmt`, `cargo clippy`, and `cargo test`.

## Security & Configuration Tips

- `ZDX_HOME` controls where config/data are stored; default is `~/.zdx`.
- Don’t commit secrets or local configs; use env vars and keep test fixtures synthetic.
