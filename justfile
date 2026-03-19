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

# Full local CI (lint + test)
ci: lint test

# Format + clippy
lint: fmt clippy

# Format (nightly rustfmt)
fmt:
    cargo +nightly fmt

# Lint
clippy:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

# Run tests (fast path, no doctests)
test:
    cargo test --workspace --lib --tests --bins

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
