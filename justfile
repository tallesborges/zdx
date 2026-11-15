# zdx justfile — run `just` to see all recipes

# Code-signing identity for `install`. Set ZDX_CODESIGN_ID in your shell env
# (e.g. "Apple Development: Name (TEAMID)") so macOS TCC grants survive
# rebuilds. Unset means the binary stays ad-hoc signed.
codesign_id := env_var_or_default("ZDX_CODESIGN_ID", "")

# Default: list available recipes
default:
    @just --list

# ─── Run ──────────────────────────────────────────

# zdx-go built from this checkout; `zdx` runs it when ZDX_GO names it.
go_bin := justfile_directory() + "/apps/bin/zdx-go"

# Run zdx from this checkout; bare `just run` starts the Go chat TUI (builds zdx-go without the Mini App; pass extra args: just run --help)
run *ARGS: go-build-dev
    ZDX_GO="{{go_bin}}" cargo run -p zdx -- {{ARGS}}

# Run the Go chat TUI directly, without zdx (turns run in the orchestrator service)
tui *ARGS: go-build-dev
    "{{go_bin}}" tui {{ARGS}}

# Run the read-only monitor from this checkout (`zdx monitor` execs zdx-go monitor). Monitor args go after `--`.
monitor *ARGS: go-build-dev
    ZDX_GO="{{go_bin}}" cargo run -p zdx -- monitor {{ARGS}}

# Run `zdx quota` from this checkout (`zdx quota` execs zdx-go quota)
quota *ARGS: go-build-dev
    ZDX_GO="{{go_bin}}" cargo run -p zdx -- quota {{ARGS}}

# Run `zdx git` from this checkout (`zdx git` execs zdx-go git; e.g. just git status)
git *ARGS: go-build-dev
    ZDX_GO="{{go_bin}}" cargo run -p zdx -- git {{ARGS}}

# The gateway's bot.pid refuses a second bot on one ZDX_HOME: stop the bot
# service first (`zdx service stop bot`). Gateway args go after `--`.

# Run the Telegram bot from this checkout (builds zdx-go with the Mini App; `zdx bot` execs zdx-go gateway)
bot *ARGS: go-build
    ZDX_GO="{{go_bin}}" cargo run -p zdx -- bot {{ARGS}}

# Run the orchestrator service (orchestrator topics in the bot need it)
orchestrator:
    cargo run -p zdx -- orchestrator serve

# Run jobs commands from this checkout (`zdx jobs` execs zdx-go jobs; pass extra args: just jobs list)
jobs *ARGS: go-build-dev
    ZDX_GO="{{go_bin}}" cargo run -p zdx -- jobs {{ARGS}}

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

# Run tests (skips doctests: there are none, and building them costs ~15s)
test:
    cargo test --workspace --lib --bins --tests

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

# Generate codebase snapshot (optional: just codebase crates/zdx-engine)
codebase *CRATES:
    cargo xtask codebase {{CRATES}}

# ─── Mini App (apps/web) ──────────────────────────

# Svelte Mini App dev server (proxies /api to a running bot's Mini App server on :4141)
web-dev *ARGS:
    cd apps/web && bun run dev {{ARGS}}

# Preview the Mini App with fixture data, no bot required
web-demo:
    @echo "http://localhost:5173/app/?view=threads&demo=1"
    cd apps/web && bun run dev

# Type-check the Mini App
web-check:
    cd apps/web && bun install --frozen-lockfile && bun run check

# Build the Mini App only when inputs or dist contents change
web-build:
    cd apps/web && bun run build-cached.mjs

# ─── Go (apps: one module, one binary zdx-go) ────

# Go module (apps/go.mod): format, vet, race tests of every package (Go toolchain pinned in go.mod)
go-check:
    cd apps && test -z "$(gofmt -l .)" && go vet ./... && go test -race ./...

# go-check for the gateway packages only (apps/gateway)
gateway-check:
    cd apps && test -z "$(gofmt -l gateway)" && go vet ./gateway/... && go test -race ./gateway/...

# go-check for the monitor packages only (apps/monitor)
monitor-check:
    cd apps && test -z "$(gofmt -l monitor)" && go vet ./monitor/... && go test -race ./monitor/...

# go-check for the chat TUI packages only (apps/tui)
tui-check:
    cd apps && test -z "$(gofmt -l tui)" && go vet ./tui/... && go test -race ./tui/...

# go-check for the shared packages and the zdx-go entry point only (apps/shared, apps/cmd)
shared-check:
    cd apps && test -z "$(gofmt -l shared cmd)" && go vet ./shared/... ./cmd/... && go test -race ./shared/... ./cmd/...

# Build zdx-go with the Mini App embedded (needs bun; default output apps/bin/zdx-go)
go-build OUT=go_bin:
    apps/build.sh "{{OUT}}"

# Build zdx-go without the Mini App (no bun; the gateway then needs --web-dist to serve one)
go-build-dev OUT=go_bin:
    apps/build.sh --no-web "{{OUT}}"

# ─── Build ────────────────────────────────────────

# Build the release zdx binary (Rust only; the Mini App ships in the gateway).
# Not incremental: .cargo/config.toml turns it on for every profile, and it gave release builds no speedup.
build-release:
    CARGO_INCREMENTAL=0 cargo build -p zdx --release

# Install current workspace as the released `zdx` binary at ~/.local/bin/zdx.
# Signed under a temp name, then renamed: a process that starts `zdx` meanwhile gets the old or the new binary, never a partial one
install: build-release
    @mkdir -p ~/.local/bin
    install -m 0755 target/release/zdx ~/.local/bin/.zdx.new
    @if [ -n "{{ codesign_id }}" ]; then \
        codesign --force --sign "{{ codesign_id }}" --identifier dev.zdx.cli ~/.local/bin/.zdx.new; \
    else \
        echo "ZDX_CODESIGN_ID unset — skipping codesign; macOS will re-prompt for permissions after each rebuild"; \
    fi
    mv -f ~/.local/bin/.zdx.new ~/.local/bin/zdx
    @echo "Installed $(~/.local/bin/zdx --version 2>/dev/null || echo zdx) to ~/.local/bin/zdx"
    @# Pay any thread-index schema rebuild here rather than inside the first
    @# request after a restart, which would otherwise block the bot. Thread
    @# index only: no exports, no memory index.
    ~/.local/bin/zdx threads reindex

# `zdx` is installed last: the new `zdx` execs zdx-go (a scheduled job runs `zdx jobs run`), so it must be in place first, and a failed build leaves the old `zdx`
# Build and install zdx-go, then zdx, then restart the launchd services
deploy: build-release go-install install
    ~/.local/bin/zdx service restart all

# Same install order as `deploy`, and queue the restart until agent runs, workers, and orchestrator updates are done
safe-deploy: build-release go-install install
    ~/.local/bin/zdx service restart all --queue

# Signed like `zdx` (the daemon's jobs run in it) so macOS TCC grants survive rebuilds; signed under a temp name, then renamed
# Build zdx-go (Mini App embedded) into ~/.local/bin/zdx-go, the path the bot service runs and `zdx` execs
go-install:
    @mkdir -p ~/.local/bin
    apps/build.sh ~/.local/bin/.zdx-go.new
    @if [ -n "{{ codesign_id }}" ]; then \
        codesign --force --sign "{{ codesign_id }}" --identifier dev.zdx.go ~/.local/bin/.zdx-go.new; \
    else \
        echo "ZDX_CODESIGN_ID unset — skipping codesign; macOS will re-prompt for permissions after each rebuild"; \
    fi
    mv -f ~/.local/bin/.zdx-go.new ~/.local/bin/zdx-go
    @echo "Installed $(~/.local/bin/zdx-go version) to ~/.local/bin/zdx-go"

# (Re)create ~/.local/bin/zdxd as a symlink to the debug build
install-debug:
    cargo build -p zdx
    @mkdir -p ~/.local/bin
    ln -sfn "$(cd target/debug && pwd)/zdx" ~/.local/bin/zdxd
    @echo "Linked ~/.local/bin/zdxd -> $(cd target/debug && pwd)/zdx"
