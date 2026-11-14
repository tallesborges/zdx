# apps/shared/gitview

The Mini App Git views (`/api/git`, `/api/git/scope`, `/api/git/diff`) in Go. The gateway calls them in process (`internal/gateway/miniapp_engine.go`). The package doc in `gitview.go` states the contract; `crates/zdx-engine/src/orchestration/view_bodies/git.rs` is the Rust implementation it replaces, still served as the `git_*` views.

## Rules

- Rust owns which thread a request means and its stored root: the caller answers the `thread_root` view (`Locate`). Never read thread files, aliases, or the thread index here, and never copy the `active` or alias rules into Go.
- Every git command goes through `capture`: `git -C <root>`, `GIT_OPTIONAL_LOCKS=0`, closed stdin, `CommandTimeout` per command, its own process group (killed with it), bounded stdout (`DiffLimitBytes` for diffs, `OutputLimitBytes` otherwise) that is closed at the deadline. A view has `RequestTimeout` in total.
- Git inherits the gateway's environment and reads the repository's own config, as the Rust views did: diffs pass `--no-ext-diff --no-textconv`, but hooks such as `core.fsmonitor` are not disabled.
- A revision reaches git only after `validCommitHash`; a diff path only after `validGitPath` and the category or scope check, after `--`, with `GIT_LITERAL_PATHSPECS=1`.
- Errors are `*Error`: the code, the exact route text, and the cause for logs only. Never put a cause in the answer.
- Bodies keep the Rust JSON values: field names, `[]` (never `null`) for empty lists, no HTML escaping (`Encode`), and Rust's lossy UTF-8 (`lossyUTF8`). Key order follows the structs (the Rust socket body had sorted keys); the golden tests compare parsed JSON.

## Tests

- `testdata/rust_views.json` holds the replies the Rust views gave on the synthetic world (`scenario_test.go`). They are the contract: rewrite them only from a Rust service, never from Go. `TestGoldenRustViews` runs every case; a case added without a recording fails it.
- Live comparison and recording (opt-in, prebuilt binary, never built here): `ZDX_SHARED_TEST_GITVIEW_RUST=<abs path to zdx> go test ./gitview -run TestRustDifferential` from `apps/shared`; add `ZDX_SHARED_TEST_GITVIEW_RECORD=1` to rewrite the recording. It starts isolated `zdx orchestrator serve` processes on temp homes.
- Fixture repositories use fixed identities and dates (stable object ids) and no user or system git config. Tests never run git in a real repository.
- The recording names the git version it came from (`git` in the file). Another git version, for example on the Linux CI runner, may print some cases differently; re-record from Rust on that version, never edit the file by hand.
