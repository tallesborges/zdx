# internal/miniapp

Go HTTP layer of the Telegram Mini App: the Svelte shell (`apps/web/dist`) and `/api/*`. It replaced the Rust Mini App server (zdx-bot's `server.rs`, now removed) route for route: `testdata/rust_routes.json` keeps that route table, and `apps/web/src/lib/api.ts` is the client contract.

## Boundaries

- Go owns: initData auth (`auth.go`), routing (`router.go`), query parsing (`query.go`), static assets and gzip (`static.go`), body and concurrency limits, short in-memory caches (monitor, quotas, usage, artifact hashes), and the `delivering` flag of a thread view.
- Rust owns every domain projection and every write, reached through `Engine` (`api.go`). Bodies pass through as `json.RawMessage`; do not add Go DTOs for engine JSON or re-derive projections, config, usage, git, or image canonicalization here.
- Feedback bodies go to the engine as text, unparsed: serde reports whichever of a type error (422) or a syntax error (400) comes first in the document, so Go cannot classify a body without the engine's schema. Go only refuses non-UTF-8 bodies and reads `submission_id` for its local in-flight guard.
- A feedback failure without an engine error reply is `ErrNotSent` (nothing happened) or `ErrOutcomeUnknown` (may still be processing; a retry reuses the submission id so the engine can recognize it, with no exactly-once promise). Untyped errors count as unknown: never tell the user it was not queued unless the adapter proved it.
- Read-only file access is limited to absolute regular files the engine resolved (`ArtifactResolve`). The engine also owns the download name, the artifact version hash, and the canonical image bytes; Go does not derive them. Every response carrying artifact bytes sends `Content-Security-Policy: sandbox`; the app reads them with fetch and renders HTML only in its own sandboxed iframe, so keep auth header-only (no cookie or query auth) and send no CORS grants.
- A missing engine capability is `ErrUnavailable` and answers 503. Never answer an empty 200 for a gap.
- Feedback refuses `active`; read routes pass `active` to the engine.
- Routing is our own, not `http.ServeMux`: the mux redirects unclean paths, answers a wrong method before auth, and matches empty segments differently from axum.

## Checks

- `go vet ./internal/miniapp && go test -race ./internal/miniapp` from `apps/gateway` (`-short` skips the 48 MiB body cases).
- `TestMain` (`sandbox_test.go`) runs every test in `internal/sandbox` (fresh HOME, ZDX_HOME, TMPDIR); the real-service tests also pass their child env through `sandbox.CheckEnv` and need an absolute binary path.
- Unit tests use fakes (`fakeEngine`, `echoEngine`, `gate`, `fstest.MapFS`) and `t.TempDir()` only: no socket, no `ZDX_HOME`, no network. Fakes do not prove engine parity.
- `golden_test.go` replays `testdata/axum_*_cases.json` against answers recorded from a Rust harness with the removed server's axum, tower-http and serde versions (pinned in `testdata/harness/Cargo.toml`) and copies of its extractors and layers (`testdata/harness`). The goldens are frozen: the Rust server is gone, so there is nothing to re-record against; the recorder's header still has the commands.
- `rust_parity_test.go` (opt-in, `ZDX_GATEWAY_RUST_ZDX=<absolute path to a zdx binary>`; never builds; skips without a binary or on missing capabilities, fails instead with `ZDX_GATEWAY_RUST_STRICT=1`) runs an isolated `zdx orchestrator serve` on a temp `HOME`/`ZDX_HOME` with a mock provider, makes one real turn, and compares every read route served by Go through the real gateway adapter with the same socket view (and `zdx views` for some). Its reference is the Rust view provider, not Rust's HTTP server. `rust_feedback_test.go` (same opt-in) sends feedback over HTTP through the real adapter into that service, with the chat's allowlist and transcription only in the project root config, and checks model requests, thread records, the renderer's routed turns and bundle markers, not only HTTP answers.
- `TestServesEveryRouteOfTheRustRouterWithItsMethod` reads the frozen Rust route table (`testdata/rust_routes.json`) and `TestServesEveryPathTheFrontendCalls` reads `api.ts`. A new route changes the Go router, `api.ts`, and that table together.
