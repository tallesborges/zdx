# apps/shared

Shared Go module `github.com/tallesborges/zdx/apps/shared` (`go.mod`; Go and toolchain pinned to the same versions as the apps). Public packages that the Go gateway (`apps/gateway`), the Go monitor (`apps/monitor`), and `zdx-quota` import. It is not part of the Cargo workspace: the Rust build needs no Go. A consumer adds `require github.com/tallesborges/zdx/apps/shared v0.0.0` and `replace github.com/tallesborges/zdx/apps/shared => ../shared` to its `go.mod`; there is no `go.work`.

## Packages

- `protocol`: orchestrator socket wire types and encodings, no I/O (`crates/zdx-engine/src/orchestration/protocol.rs` and `views.rs` are the source of truth). Status report and capabilities, `ServiceError`, `ErrUnsupported`, view ops with their capability and reply budget, `View`, `DecodeViewReply` (socket reply and `zdx views` stdout), `EncodeLine` (op-tagged request line, no HTML escaping).
- `socket`: the transport. One request line per Unix socket connection; `Call` reads one reply line, `Open` returns a `Stream` of raw reply lines. It never merges, drops, or reorders lines: a client that coalesces streaming deltas does it above this package. Cancelling the context closes the connection. Every transport error wraps exactly one outcome class: `ErrRequestNotSent` (nothing written), or `ErrRequestOutcomeUnknown` (`ErrNoAnswer` when the budget or the caller's context ended; it keeps its text and also wraps the context error). `Status`, `Require`, and `View` ask for status on every call, because the service can restart onto another build.
- `quota`: live subscription quotas of every stored provider account, the one implementation for `zdx quota`, the Mini App, and the monitor. See the package doc in `quota/quota.go`.
- `version`: the zdx version a Go program was built for (`version.Zdx`, set by the linker flag `ldflags.sh` prints from the workspace `Cargo.toml`). `Resolve(explicit)` never guesses: without a flag value or a build value it returns `ErrUnknown`, which says how to build or run with a version.
- `sandbox`: `TestMain` isolation for this module's tests (fresh `HOME`, `ZDX_HOME`, `TMPDIR`; other `ZDX_` variables and `OPENCODE_API_KEY` dropped; a failing `zdx` on PATH; the run fails when the inherited home's OAuth cache or live-service files changed).
- `cmd/zdx-quota`: the quota reader `zdx quota [--json]` execs (`crates/zdx-cli/src/cli/commands/quota.rs`, `GoProgram` contract): `--root ROOT --zdx-version VERSION [--json]`. Output is the stdout `zdx quota` printed when it was Rust. It never runs `zdx`.

## Rules

- Quota reads credentials read-only: never refresh a token, write `oauth.json`, or take `oauth.lock`. Rust is the only writer.
- Runtime environment: credentials come from `$ZDX_HOME/oauth.json`, and OpenCode Go from `OPENCODE_API_KEY` in the environment of the process that reads quotas (the bot service for the Mini App, the shell for `zdx quota` and the monitor). launchd services get no `OPENCODE_API_KEY` unless it is set for launchd.
- Never log or print a token, a credential field, or a provider response body. Errors name files and fields, not values.
- A failure to read the account store is an error (`quota.ErrDiscovery`), never an empty healthy list. A missing `oauth.json` is no accounts.
- Tests use synthetic homes, `httptest` servers, fake credentials, and an explicit synthetic `Version`. No real home, credential, provider endpoint, or live service. Create Unix sockets under `os.MkdirTemp("", ...)`: macOS limits socket paths to 104 bytes.

## Build and checks

- `just shared-check`: `gofmt -l .`, `go vet ./...`, `go test -race ./...` from `apps/shared`. GHA runs the same commands on Linux (`go-shared` job, Go from this `go.mod`'s toolchain line), so keep tests portable.
- `just quota-build [OUT]` (default `apps/shared/bin/zdx-quota`), `just quota-install` (`~/.local/bin/zdx-quota`, which `zdx quota` runs; `just deploy` and `just safe-deploy` install it), `just quota [ARGS]` (build, then `zdx quota` from this checkout).
- Deploy together: Rust has no quota view, so an engine (`zdx`) installed without the new gateway, `zdx-quota`, and monitor leaves older installed Go binaries asking for the retired `subscription_quotas` view: their quotas stay `pending`/stale (gateway) or show an error (monitor). `just deploy` / `just safe-deploy` install `zdx`, the gateway, and `zdx-quota` and refresh an installed monitor in one step; use them, not `just install` alone.
- Every build recipe of a program that reports a version passes `-ldflags "$(apps/shared/ldflags.sh)"`. With `go run`, add the same flag (from `apps/monitor`: `../shared/ldflags.sh`) or pass `--zdx-version`.
- Opt-in Rust check against a built reader: `ZDX_QUOTA_TEST_BIN=<abs path to zdx-quota> cargo test -p zdx --test integration quota`.
