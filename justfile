# zdx justfile — run `just` to see all recipes

# Code-signing identity for `install`. Set ZDX_CODESIGN_ID in your shell env
# (e.g. "Apple Development: Name (TEAMID)") so macOS TCC grants survive
# rebuilds. Unset means the binary stays ad-hoc signed.
codesign_id := env_var_or_default("ZDX_CODESIGN_ID", "")

# Default: list available recipes
default:
    @just --list

# ─── Run ──────────────────────────────────────────

# Run the TUI (pass extra args: just run --help)
run *ARGS:
    cargo run -p zdx -- {{ARGS}}

# Run the service dashboard
monitor:
    cargo run -p zdx -- monitor

# Run the Telegram bot
bot:
    cargo run -p zdx -- bot

# Run automations commands (pass extra args: just automations list)
automations *ARGS:
    cargo run -p zdx -- automations {{ARGS}}

# ─── Quality ──────────────────────────────────────

# Full local CI (lint + test) — use before pushing
ci: lint test

# Fast inner-loop check — single cargo mode (clippy), default features, lib+bins only
ci-fast: fmt
    cargo clippy --workspace -- -D warnings

# Format + clippy
lint: fmt clippy

# Format (nightly rustfmt)
fmt:
    cargo +nightly fmt

# Install git hooks (pre-commit formats Rust; pre-push lints Rust-related changes)
install-hooks:
    git config core.hooksPath .githooks
    @echo "Installed: pre-commit formatting and change-aware pre-push Clippy"

# Lint
clippy:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

# Run tests with nextest (fast path, no doctests)
test:
    cargo nextest run --workspace

# ─── Xtask ────────────────────────────────────────

# Update default models
update-models:
    cargo xtask update-default-models

# Update default config
update-config:
    cargo xtask update-default-config

# Update both defaults
update-defaults:
    cargo xtask update-defaults

# Generate codebase snapshot (optional: just codebase crates/zdx-tui)
codebase *CRATES:
    cargo xtask codebase {{CRATES}}

# ─── Build ────────────────────────────────────────

# Build release binary
build-release:
    cargo build -p zdx --release

# Install current workspace as the released `zdx` binary at ~/.local/bin/zdx
install: build-release
    @mkdir -p ~/.local/bin
    install -m 0755 target/release/zdx ~/.local/bin/zdx
    @if [ -n "{{ codesign_id }}" ]; then \
        codesign --force --sign "{{ codesign_id }}" --identifier dev.zdx.cli ~/.local/bin/zdx; \
    else \
        echo "ZDX_CODESIGN_ID unset — skipping codesign; macOS will re-prompt for permissions after each rebuild"; \
    fi
    @echo "Installed $(~/.local/bin/zdx --version 2>/dev/null || echo zdx) to ~/.local/bin/zdx"

# Build, install, and restart the launchd services with the new binary
deploy: install
    ~/.local/bin/zdx service restart all

# (Re)create ~/.local/bin/zdxd as a symlink to the debug build
install-debug:
    cargo build -p zdx
    @mkdir -p ~/.local/bin
    ln -sfn "$(cd target/debug && pwd)/zdx" ~/.local/bin/zdxd
    @echo "Linked ~/.local/bin/zdxd -> $(cd target/debug && pwd)/zdx"
