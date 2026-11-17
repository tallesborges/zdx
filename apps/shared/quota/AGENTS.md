# apps/shared/quota

Live subscription quotas (Claude, Codex, Antigravity, Grok Build, Muse Code) and the shared CLI and view converters. Go port of the Rust quota readers (`crates/zdx-providers/src/subscription_quota.rs`), `zdx quota` output (`crates/zdx-cli/src/cli/commands/quota.rs`), and the subscription part of `crates/zdx-engine/src/orchestration/view_bodies/monitor.rs`, all at commit `aab08ba4`. The Rust readers are removed; that commit is the reference.

## Rules

- Credentials: Fetch reads `oauth.json` and refreshes expired or soon-to-expire (within 5 min) access tokens with their refresh tokens under `oauth.lock` (atomic rename), so the monitor never shows `expired` while a refresh token is valid. Muse Code re-mints its key on every quota fetch, so it is covered by the same invariant; a 401 from its mint maps to `KindExpired` (re-login).
- Errors stay bounded: an account failure is an `Error` kind, a discovery failure wraps `ErrDiscovery` with the file path and a cause that never quotes file contents. Never put a response body, token, header value, or URL in an error or log.
- `testdata/differential/` is the parity contract: 62 raw provider bodies with the output the Rust readers recorded. `recorder.rs.txt` there is the reconstructed recorder source with the commit, crate versions, and steps to re-record. Never rewrite a `.out.json` from Go. A parser change must keep every case passing.
- Decoding (`wire.go`) follows serde derive plus `serde_json::from_slice` where the fixtures and tests pin it; the package comment there lists the rules. Do not swap it for `encoding/json` struct decoding.
- `rustfmt.go` ports chrono RFC 3339 parse/format (leap seconds kept with the `leapSecond` mark), serde_json (zmij) float text, Rust `f64::from_str`, and `{:.N}`. Change them only with a test that pins the Rust behavior.
- The default client follows reqwest: one connect budget for TCP, proxy tunnel, and TLS (`transport.go`), proxies from `ALL_PROXY`/`HTTPS_PROXY`/`HTTP_PROXY`/`NO_PROXY` read through `LookupEnv` with hyper-util's rules, no compression. `Fetch` forces reqwest's redirect policy on every client (max 10, drop credentials once any hop changes scheme, host, or port, for every later hop too): Go alone forwards Authorization to subdomains, and it copies the first request's headers onto each hop.
- Consumers (CLI, gateway, monitor) call `Fetch` and the converters; they do not re-derive names, clamping, sorting, or reasons, and they print resets with `FormatRFC3339`.

## Known differences from Rust (accepted)

- serde's array form for a struct (`"config":[...]`, or an `oauth.json` entry written as an array) is accepted by Rust and rejected here.
- Values skipped as unknown fields are unchecked as in Rust, except that `encoding/json` rejects nesting deeper than 10000 levels anywhere in a body.
- A leap second (`:60`) is stored as the next second in the `leapSecond` location. Comparisons and durations use that instant, which can be one second off chrono's leap-aware subtraction; the CLI countdown (`chronoSeconds`) applies chrono's rule and is pinned by chrono output. Only `FormatRFC3339` prints `:60`, and `UTC()`/`In()` drop the mark.
- Discovery: a missing `oauth.json` (or a dangling link) is no accounts. Any other failure to read it (permissions on the file or a parent, a symlink loop, a home that is a file) is `ErrDiscovery`; Rust's `Path::exists` read those as no accounts, so every provider showed `not logged in`.
- Hardening, not parity: provider bodies over `MaxResponseBytes` (8 MiB) are `unexpected response`; Rust had no cap. `oauth.json` has no cap, as in Rust.
- Antigravity sends `antigravity/cli/2.24.0`, the zdx model client's UA, not Rust's `antigravity/cli/1.0.0`. On 2026-11-14 the quota summary answered `1.0.0` with only the Claude and GPT group, so the Gemini filter gave `unexpected response`; `2.24.0` got the Gemini groups back (public reports from 2026-10 describe the same for other legacy Antigravity UAs). The removed Rust reader sent the same UA and would have failed the same way (not tested). Parsing is unchanged. Keep the UA in step with `crates/zdx-providers/src/gemini/antigravity.rs`.
- Muse Code: an active subscription answer with no `subs_usage` and no payment demand is a 0% `rolling` window with no reset, not `unexpected response` (the differential case `muse-code/no-usage` keeps the Rust record; `acceptedDifferences` in `differential_test.go` holds the Go answer). On 2026-11-15 the mint answer had no `subs_usage` before the first model request of the day and had it right after (a fresh 5h window at 0%); oh-my-pi reads the same gap as an idle window. The weekly figure is not in that answer, so it is not reported.
- Proxies: an `http://` target (tests only; production endpoints are HTTPS) goes only through an `http` proxy, never an `https` or socks one. A socks proxy fails the connection, as the Rust build without reqwest's `socks` feature does, but the exact failure was not compared. Proxy URLs parse with `net/url`, not the `http` crate, so odd URLs may differ. The `https://` proxy path (TLS to the proxy) has no test.
- Checked against the serde_json source only, with no recorded Rust output: the `oauth.json` rules (serde flatten checks every string, number, and the 128-level nesting limit) and the Muse Code `resets_at` nesting limit.

## Tests

- `TestMain` uses `sandbox.Main`. Tests use temp homes, `httptest`, and an explicit `LookupEnv`; never the real `ZDX_HOME`, real credentials, or real endpoints. Never leave `Options.Endpoints` empty in a test that has credentials.
- Goldens in `testdata/` (`cli_quota*.json`, `cli_quota.txt`, `subscriptions.json`) are derived from the Rust sources by hand. The lead compared empty-home CLI JSON and text with the Rust binary; the other goldens were not produced by Rust. Rewrite with `go test ./quota -run Golden -update` and review the diff line by line.
- Checks, from `apps/shared`: `gofmt -l quota`, `go vet ./quota`, `go test -race ./quota`.
