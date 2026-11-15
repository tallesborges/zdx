# zdx justfile — run `just` to see all recipes

# Code-signing identity for `install`. Set ZDX_CODESIGN_ID in your shell env
# (e.g. "Apple Development: Name (TEAMID)") so macOS TCC grants survive
# rebuilds. Unset means the binary stays ad-hoc signed.
codesign_id := env_var_or_default("ZDX_CODESIGN_ID", "")

# Default: list available recipes
default:
    @just --list

# ─── Run ──────────────────────────────────────────

# Run zdx from this checkout; bare `just run` starts the Go chat TUI it builds (pass extra args: just run --help)
run *ARGS: tui-build
    ZDX_TUI="{{justfile_directory()}}/apps/tui/bin/zdx-tui" cargo run -p zdx -- {{ARGS}}

# Run the built Go chat TUI directly, without zdx (turns run in the orchestrator service)
tui *ARGS: tui-build
    apps/tui/bin/zdx-tui {{ARGS}}

# Run the read-only monitor from this checkout (builds the Go monitor; `zdx monitor` execs it). Monitor args go after `--`.
monitor *ARGS: monitor-build
    ZDX_MONITOR="{{justfile_directory()}}/apps/monitor/bin/zdx-monitor" cargo run -p zdx -- monitor {{ARGS}}

# Run `zdx quota` from this checkout (builds the Go quota reader; `zdx quota` execs it)
quota *ARGS: quota-build
    ZDX_QUOTA="{{justfile_directory()}}/apps/shared/bin/zdx-quota" cargo run -p zdx -- quota {{ARGS}}

# The gateway's bot.pid refuses a second bot on one ZDX_HOME: stop the bot
# service first (`zdx service stop bot`). Gateway args go after `--`.

# Run the Telegram bot from this checkout (builds the Go gateway; `zdx bot` execs it)
bot *ARGS:
    apps/gateway/build.sh
    ZDX_GATEWAY="{{justfile_directory()}}/apps/gateway/bin/zdx-gateway" cargo run -p zdx -- bot {{ARGS}}

# Run the orchestrator service (orchestrator topics in the bot need it)
orchestrator:
    cargo run -p zdx -- orchestrator serve

# Run jobs commands from this checkout (builds the Go jobs program; `zdx jobs` execs it; pass extra args: just jobs list)
jobs *ARGS: jobs-build
    ZDX_JOBS="{{justfile_directory()}}/apps/shared/bin/zdx-jobs" cargo run -p zdx -- jobs {{ARGS}}

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

# Go gateway (apps/gateway): format, vet, race tests (Go toolchain pinned in its go.mod)
gateway-check:
    cd apps/gateway && test -z "$(gofmt -l .)" && go vet ./... && go test -race ./...

# Build zdx-gateway with the Mini App embedded (default output apps/gateway/bin/zdx-gateway)
gateway-build *OUT:
    apps/gateway/build.sh {{OUT}}

# Go monitor (apps/monitor): format, vet, race tests (Go toolchain pinned in its go.mod)
monitor-check:
    cd apps/monitor && test -z "$(gofmt -l .)" && go vet ./... && go test -race ./...

# Build zdx-monitor (Go only, no cargo; default output apps/monitor/bin/zdx-monitor)
monitor-build OUT=(justfile_directory() + "/apps/monitor/bin/zdx-monitor"):
    cd apps/monitor && go build -trimpath -ldflags "$(../shared/ldflags.sh)" -o "{{OUT}}" ./cmd/zdx-monitor

# Go chat TUI (apps/tui): format, vet, race tests (Go toolchain pinned in its go.mod)
tui-check:
    cd apps/tui && test -z "$(gofmt -l .)" && go vet ./... && go test -race ./...

# Build zdx-tui (Go only, no cargo; default output apps/tui/bin/zdx-tui)
tui-build OUT=(justfile_directory() + "/apps/tui/bin/zdx-tui"):
    cd apps/tui && go build -trimpath -o "{{OUT}}" ./cmd/zdx-tui

# Shared Go module (apps/shared: socket, protocol, transcript, quota, jobs, zdx-quota, zdx-jobs): format, vet, race tests
shared-check:
    cd apps/shared && test -z "$(gofmt -l .)" && go vet ./... && go test -race ./...

# Build zdx-quota, the reader `zdx quota` runs (Go only, no cargo; default output apps/shared/bin/zdx-quota)
quota-build OUT=(justfile_directory() + "/apps/shared/bin/zdx-quota"):
    cd apps/shared && go build -trimpath -ldflags "$(./ldflags.sh)" -o "{{OUT}}" ./cmd/zdx-quota

# Build zdx-jobs, the jobs program `zdx jobs` runs (Go only, no cargo; default output apps/shared/bin/zdx-jobs)
jobs-build OUT=(justfile_directory() + "/apps/shared/bin/zdx-jobs"):
    cd apps/shared && go build -trimpath -o "{{OUT}}" ./cmd/zdx-jobs

# ─── Build ────────────────────────────────────────

# Build the release zdx binary (Rust only; the Mini App ships in the gateway).
# Not incremental: .cargo/config.toml turns it on for every profile, and it gave release builds no speedup.
build-release:
    CARGO_INCREMENTAL=0 cargo build -p zdx --release

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
    @# Pay any thread-index schema rebuild here rather than inside the first
    @# request after a restart, which would otherwise block the bot. Thread
    @# index only: no exports, no memory index.
    ~/.local/bin/zdx threads reindex

# Build and install zdx, the gateway, zdx-quota, zdx-jobs, and zdx-tui (and refresh an installed monitor), then restart the launchd services
deploy: install gateway-install quota-install jobs-install tui-install monitor-refresh
    ~/.local/bin/zdx service restart all

# Build and install zdx, the gateway, zdx-quota, zdx-jobs, and zdx-tui (and refresh an installed monitor), and queue the restart until agent runs, workers, and orchestrator updates are done
safe-deploy: install gateway-install quota-install jobs-install tui-install monitor-refresh
    ~/.local/bin/zdx service restart all --queue

# Build the Go gateway into ~/.local/bin/zdx-gateway, the stable path the bot service runs
gateway-install:
    @mkdir -p ~/.local/bin
    apps/gateway/build.sh ~/.local/bin/.zdx-gateway.new
    mv -f ~/.local/bin/.zdx-gateway.new ~/.local/bin/zdx-gateway
    @echo "Installed ~/.local/bin/zdx-gateway"

# Build the Go monitor into ~/.local/bin/zdx-monitor, the path `zdx monitor` runs
monitor-install:
    @mkdir -p ~/.local/bin
    cd apps/monitor && go build -trimpath -ldflags "$(../shared/ldflags.sh)" -o ~/.local/bin/.zdx-monitor.new ./cmd/zdx-monitor
    mv -f ~/.local/bin/.zdx-monitor.new ~/.local/bin/zdx-monitor
    @echo "Installed ~/.local/bin/zdx-monitor"

# Build the Go chat TUI into ~/.local/bin/zdx-tui, the path bare `zdx` runs
tui-install:
    @mkdir -p ~/.local/bin
    cd apps/tui && go build -trimpath -o ~/.local/bin/.zdx-tui.new ./cmd/zdx-tui
    mv -f ~/.local/bin/.zdx-tui.new ~/.local/bin/zdx-tui
    @echo "Installed ~/.local/bin/zdx-tui"

# Build the Go quota reader into ~/.local/bin/zdx-quota, the path `zdx quota` runs
quota-install:
    @mkdir -p ~/.local/bin
    cd apps/shared && go build -trimpath -ldflags "$(./ldflags.sh)" -o ~/.local/bin/.zdx-quota.new ./cmd/zdx-quota
    mv -f ~/.local/bin/.zdx-quota.new ~/.local/bin/zdx-quota
    @echo "Installed ~/.local/bin/zdx-quota"

# Build zdx-jobs into ~/.local/bin/zdx-jobs, which `zdx jobs` (and so the daemon service) runs
jobs-install:
    @mkdir -p ~/.local/bin
    cd apps/shared && go build -trimpath -o ~/.local/bin/.zdx-jobs.new ./cmd/zdx-jobs
    mv -f ~/.local/bin/.zdx-jobs.new ~/.local/bin/zdx-jobs
    @echo "Installed ~/.local/bin/zdx-jobs"

# Rebuild ~/.local/bin/zdx-monitor only when it is installed: deploys never install it
monitor-refresh:
    @if [ -e ~/.local/bin/zdx-monitor ]; then {{just_executable()}} monitor-install; else echo "zdx-monitor not installed, skipped (run just monitor-install once)"; fi

# (Re)create ~/.local/bin/zdxd as a symlink to the debug build
install-debug:
    cargo build -p zdx
    @mkdir -p ~/.local/bin
    ln -sfn "$(cd target/debug && pwd)/zdx" ~/.local/bin/zdxd
    @echo "Linked ~/.local/bin/zdxd -> $(cd target/debug && pwd)/zdx"
