# apps/shared/gitview

The Mini App Git views (`/api/git`, `/api/git/scope`, `/api/git/diff`) in Go. The gateway calls them in process (`internal/gateway/miniapp_engine.go`). The package doc in `gitview.go` states the contract. It is the only implementation: the Rust `git_*` views it replaced are retired, and the socket refuses their ops.

## Rules

- Rust owns which thread a request means and its stored root: the caller answers the `thread_root` view (`Locate`). Never read thread files, aliases, or the thread index here, and never copy the `active` or alias rules into Go.
- Every git command goes through `capture`: `git -C <root>`, `GIT_OPTIONAL_LOCKS=0`, closed stdin, `CommandTimeout` per command, its own process group (killed with it), bounded stdout (`DiffLimitBytes` for diffs, `OutputLimitBytes` otherwise) that is closed at the deadline. A view has `RequestTimeout` in total.
- Git inherits the gateway's environment and reads the repository's own config, as the Rust views did: diffs pass `--no-ext-diff --no-textconv`, but hooks such as `core.fsmonitor` are not disabled.
- A revision reaches git only after `validCommitHash`; a diff path only after `validGitPath` and the category or scope check, after `--`, with `GIT_LITERAL_PATHSPECS=1`.
- Errors are `*Error`: the code, the exact route text, and the cause for logs only. Never put a cause in the answer.
- Bodies keep the Rust JSON values: field names, `[]` (never `null`) for empty lists, no HTML escaping (`Encode`), and Rust's lossy UTF-8 (`lossyUTF8`). Key order follows the structs (the Rust socket body had sorted keys); the golden tests compare parsed JSON.

## Tests

- `testdata/rust_views.json` holds the replies the retired Rust views gave on the synthetic world (`scenario_test.go`), recorded before they were removed. They are the contract and cannot be recorded again: never rewrite them from Go or by hand, and keep `viewCases` as recorded. `TestGoldenRustViews` runs every case. Test new or changed behavior with ordinary tests (`gitview_test.go`).
- To investigate an old reply, build the last Rust binary with the `git_*` views (the parent of the commit that deleted `crates/zdx-engine/src/orchestration/view_bodies/git.rs`) in a separate worktree.
- Fixture repositories use fixed identities and dates (stable object ids) and no user or system git config. Tests never run git in a real repository.
- The recording names the git version it came from (`git` in the file). Another git version, for example on the Linux CI runner, may print some cases differently. Check the case against that git's output; never edit the file by hand.
