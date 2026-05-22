# zdx justfile — run `just` to see all recipes

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

# Run the bot with auto-rebuild (exit 42 = rebuild)
bot-loop:
    #!/usr/bin/env bash
    set -euo pipefail
    while true; do
        echo "🚀 Starting bot..."
        cargo run -p zdx -- bot || EXIT_CODE=$?
        EXIT_CODE=${EXIT_CODE:-0}
        if [ "$EXIT_CODE" -eq 42 ]; then
            echo "♻️  Restart requested, rebuilding..."
            continue
        fi
        echo "Bot exited with code $EXIT_CODE"
        break
    done

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
    install -m 0755 ../.zdx/cargo-target/release/zdx ~/.local/bin/zdx
    @echo "Installed $(~/.local/bin/zdx --version 2>/dev/null || echo zdx) to ~/.local/bin/zdx"

# (Re)create ~/.local/bin/zdxd as a symlink to the debug build
install-debug:
    cargo build -p zdx
    @mkdir -p ~/.local/bin
    ln -sfn "$(cd ../.zdx/cargo-target/debug && pwd)/zdx" ~/.local/bin/zdxd
    @echo "Linked ~/.local/bin/zdxd -> $(cd ../.zdx/cargo-target/debug && pwd)/zdx"
