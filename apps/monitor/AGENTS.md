# apps/monitor

ZDX dashboard in Go with Bubble Tea v2 (`charm.land/bubbletea/v2`, Lip Gloss v2). Part of the one Go module `apps/go.mod` (the `monitor` command of `zdx-go`), not part of the Cargo workspace: Rust builds never need Go, and the module never needs cargo. It imports `apps/shared` (socket transport, wire types, quotas, build version). It is what `zdx monitor` runs; the Rust monitor crate it replaced is gone. It is read-only except Config tab edits, which Rust checks and writes (`zdx config set-setting|reset-setting`; see Surfaces and gaps).

## Running it

- From the repo root: `just monitor [-- ARGS]` (build `apps/bin/zdx-go`, then `zdx monitor` execs it), `just monitor-check`. `just deploy`/`safe-deploy` install `~/.local/bin/zdx-go`. CI vets and compiles the monitor but does not run its tests.
- `zdx monitor [-- ARGS]` execs `$ZDX_GO` (absolute) or `~/.local/bin/zdx-go` as `zdx-go monitor --root ROOT ARGS`, with `ZDX_HOME` and `ZDX_BIN` set (`crates/zdx-cli/src/cli/commands/go_program.rs`, the same contract as `zdx bot`). Never a PATH lookup.
- Layout preview without a home, service, or binary: from `apps`, `go run ./monitor/cmd/zdx-monitor-preview --scenario full|empty|down|loading|stale|scope` (synthetic `internal/fixture` data).
- Run: `zdx-go monitor [--root DIR] [--zdx-home DIR] [--zdx PATH] [--zdx-version V] [--cli-fallback]`. Defaults: root `.`, home `$ZDX_HOME` else `~/.zdx`, binary `$ZDX_BIN` else `zdx` on PATH (a missing binary only breaks the tabs that need the CLI). Pass the engine service's own `--root` (its `views_root`); on a mismatch the Usage and Config footers show `! scope ROOT` and `?` help explains it, because those answers then belong to the service's root.
- zdx version (quota user agent): `--zdx-version`, else the build-time `version.Zdx` (`apps/shared/version.Resolve`); nothing guesses it. Builds set it with `apps/shared/ldflags.sh` (the lead owns the `just` recipes). A plain `go build`/`go run` has none: the Usage tab then shows how to fix it in the quota section, and every other surface still works. Development runs: `just monitor`; from `apps`, `go run -ldflags "$(shared/ldflags.sh)" ./cmd/zdx-go monitor`, or `go run ./cmd/zdx-go monitor --zdx-version <version>`.
- It never starts a bot, an engine, or a service, and the monitor itself writes no ZDX state: a Config edit runs `zdx config set-setting|reset-setting`, which writes the file. Reading is not free of disk writes: the engine views it calls may bring derived caches (thread index, usage cache) up to date, and the service drops stale run markers. Quotas read `$ZDX_HOME/oauth.json` (read-only; no token refresh, no write) and `OPENCODE_API_KEY` from the monitor's own environment, then call each provider over HTTPS from the monitor process.

## Boundaries

- Rust owns every rule: config resolution, usage accounting (`usage_report` stays a Rust view), worker and run state, job status, indexed thread queries, service control. Subscription quotas are the exception: `apps/shared/quota` owns them (`Fetch`, then `Subscriptions` for the rows). Go decodes and lays out what Rust or the shared module returns. Do not port engine rules here; ask for a socket view (with its `zdx views` twin) instead.
- Views come from the engine socket (`$ZDX_HOME/run/engine.sock`, NDJSON, one request per connection; source of truth `crates/zdx-engine/src/orchestration/protocol.rs` and `views.rs`). The client checks the view capability with a `status` call before every view, as the gateway does. It sends only `status` and view ops.
- `--cli-fallback` runs views through `zdx --root ROOT views --file -` while the socket has no listener. Off by default: an in-process view refreshes derived caches (thread index, usage cache) in that process. The footer marks such answers `via zdx views`, and `?` help says it is on.
- Config edits (`ui/config_edit.go`, optional `ui.ConfigWriter` on the backend): the value list is the catalog model picker of the scope's root (`zdx helper`, kind `picker`, the same data as the chat TUI and bot `/model`; the global scope uses `--root`), then `zdx config set-setting|reset-setting --dry-run --json` shows the reach, and `↵` runs it without `--dry-run`. The monitor composes only the value (a spec, `on`/`off`, or for a mode row the mode list with that primary replaced); Rust validates and writes.
- Read-only CLI JSON: `zdx service status --json` (service state without the engine; reads pidfiles and launchd) and `zdx jobs list --scheduler --json` (`job_scheduler::jobs()`). Children get `ZDX_HOME` set to the monitor's home.
- Logs: the tail (at most 1 MiB, then the last 2000 lines) of `$ZDX_HOME/logs/zdx.log*`. No view exists for them; no other directory is listed.
- No directory scans for corpus data and no fallback that invents data. Missing data stays unknown, stale, or an error on screen.
- Code the Go programs share lives in `apps/shared` (`protocol`, `socket`, `transcript`, `quota`, `version`); coordinate changes there with its owner. Do not import `apps/gateway/internal/...`.
- Quotas (`source.Quotas`): no service needed and no cache; each fetch is one `quota.Fetch` (on entry to Usage, `Q`, every 5 min). An unknown version, an unreadable `oauth.json` (`quota.ErrDiscovery`), or a cancelled or timed-out fetch is an error: the last rows stay, marked stale, never an empty healthy list. A failed account is a row with its reason; accounts that are not logged in are left out.

## Surfaces and gaps

| Tab | Source | Not built (the removed Rust monitor had it) |
| --- | --- | --- |
| Agents | `active_agents` as a tree + `thread_view` transcript (cursor polls, merged as `apps/web` merges) + `i` overlay over `thread_trajectory` | thinking toggles all blocks, not one; no mouse click on a tool; no expand/collapse |
| Threads | `threads_list` (recent 50, `all`/`workers`) | indexed browse (500 rows, kind/project/text filters, project picker); `o` open JSONL |
| Services | `zdx service status --json` + socket `status` threads | controls (not built; CLI exists, see below) |
| Jobs | `zdx jobs list --scheduler --json` | model edit, archived list (CLI exists) |
| Usage | `usage_report` (`[`/`]` days) + in-process `apps/shared/quota` (`quota.Fetch`, on entry, `Q`, every 5 min) | charts are one sparkline |
| Logs | rolling log files | target filter picker, copy, line overlay |
| Background | `monitor_snapshot.background_processes` | detail log tails; kill (CLI exists) |
| Config | `workspaces` (global scope + workspaces, one row per mode; `/` finds a scope by typing, `[ ]` step) + `monitor_snapshot.config`; edits through `zdx config set-setting|reset-setting` | mode name, description, and alternatives edits; adding or removing a mode; override counts |

- Agents tree (`agent_tree.go`): a run nests under the running run whose `thread_id` equals its marker's `parent_thread_id` (a worker's owner, a subagent's or helper's caller). A parent with no run is a dim context row (not counted, STATE `idle`, `↵` opens its thread). Its RUN is the thread title and its MODEL the last request's `provider:model` (no thinking level: a request does not record one), both from `thread_view` (`parents.go`): one full read when the parent first appears, then `after`-cursor deltas every 15 s while the Agents tab is shown, at most `maxParentLookups` threads per fetch (never-read first, then the one answered longest ago, so every parent gets its turn). A parent the tree stops showing (it runs, or its children end) keeps its cursor (at most `maxKeptParents` such), and a look from that cursor runs as soon as it is shown again; a reset cursor or moved alias reads it all again, and an answer for an entry that changed while the look ran is dropped. Unknown stays `—` (no request yet, `not_found`); a failed look keeps older data and says so in the detail. The model a new turn would get is not shown: catalog `thread_status` resolves it only against the chat's root, which the monitor does not know; links that would close a cycle are dropped; siblings keep the engine's order. No nesting is inferred from role, model, root, or order. The MODEL column is one spec, `provider[@account]:model@thinking`, built from the marker's `provider`, `account`, `model`, and `thinking` (`specParts`). A model that starts with `provider:` or `provider@account:` keeps its own prefix; the level is not added again when the model already has it as an `@level` suffix. Any other `@` or `:` in a model id is opaque, and a missing part is left out. Narrow columns drop the account, then cut the provider at the provider boundary before the `model@thinking` end (`shortSpec`); the detail pane shows the full spec and each part.
- List columns show one unit of an engine duration (`compactAge`: `1h 23m` → `1h`) so a column never cuts a duration; details show the full value.
- Transcript merge: a delta whose cursor went back (thread reset) or whose `source_id` changed (alias moved) is dropped and the thread loads in full again. A `tool_running` item with `completed_at` is a finished call whose round is not saved yet; it shows its `ok` outcome (unknown when absent), never "running". Message attachments (`artifacts`) show as name, kind, and path.
- Transcript layout, Markdown, and text cleaning are the shared `apps/shared/transcript` package (also the chat TUI's); `data` aliases its thread types. Its rules are in `apps/shared/AGENTS.md`.
- External text is untrusted: `applyResult` runs `transcript.CleanAll` on every answer, and `oneLine` cleans error strings, so escape sequences (SGR, OSC 8, DCS/APC passthrough), C0/C1 controls, and carriage returns never reach the renderer. Bubble Tea's renderer applies SGR and hyperlinks and forwards terminated DCS/APC/SOS/PM sequences, so do not rely on it.
- `render` never returns a frame larger than the terminal; below 20x6 it shows a short "terminal too small" frame.
- `thread_trajectory` does not follow an alias (`thread_view` does, and returns `source_id`). The overlay asks for the transcript's `source_id`, so it waits for the first `thread_view` answer.

Existing engine interfaces for the missing parts (reuse them when controls are approved; none is wired yet):

- Service start/stop/restart: `zdx service start|stop|restart [SERVICE]`. Stop and restart refuse active agent runs and in-flight engine work unless `--force`; `restart --queue` queues it.
- Background kill: `zdx bg kill <bg_id>`. Exited processes: `zdx bg list --json`.
- Archived once jobs: `zdx jobs list --archived --json`.
- Job model edit: `zdx jobs set-model NAME SPEC|--clear`. Missing: the compare-and-set guard `jobs::set_model` supports (`expected`); the CLI passes `None`.
- Model picker list: `zdx models list --json`.

Genuinely missing (each needs a JSON CLI or view in Rust):

- Indexed thread browse: `thread_index::browse_threads` (kind, project, text, 500 rows) and `browse_projects` have no CLI or view. `zdx threads search --json` is FTS plus dates only; `threads_list` is the 50 most recent.
- Background detail: `bg list --json` and `monitor_snapshot` carry no stdout/stderr log paths or tails (`background_activity::stdout_log_path`, `read_log_tail`).

## Layout

- `cli`: `zdx-go monitor` (`Main`): flags and `tea.NewProgram` with a cancellable context.
- `cmd/zdx-monitor-preview`: the layout preview on fixture data (a development command, not in `zdx-go`).
- `internal/wire`: the monitor's read-only face of `apps/shared/socket` and `protocol` (aliases, `Status`, `View`; capability check, reply budgets, and cancel are the shared client's). It changes two error texts and keeps the shared error in the chain: a service error shows `code: message`, and a missing reply shows "the engine service did not answer in time" (still `socket.ErrNoAnswer`, wrapping the context error).
- `internal/data`: JSON body types, decode only. `Subscription` and `QuotaWindow` are aliases of the shared quota rows.
- `internal/source`: the production backend: socket views, optional CLI fallback, direct quotas, CLI JSON, log tail.
- `internal/ui`: Bubble Tea model. `chrome.go`: no header row; the tab bar is the first line (fits by dropping badges, then long labels). The footer has the hints that fit on the left and, on the right, freshness, `! scope ROOT` (Usage and Config only, when the service's `views_root` is not `--root`), the socket connection (`connected`, `connected, views missing`, `disconnected`, `unreachable`, `connecting`; never agent busyness), and `? help`; it drops freshness, then the scope path, then words, never the scope mark or the connection dot. A stale surface gets its own footer line. The `?` help has paths, protocol, capabilities, the full scope explanation, and what needs the engine. `layout.go`: the shared `table` (auto-sized and flex columns, `minWidth` hiding, `cutLeft`, selection background that keeps cell colors), `listWithDetail`, duration and time formatting. `fetch.go` is the refresh loop: one fetch per key at a time, a generation per key so an answer for an older request (other thread, kind, days, log file) is dropped, and `invalidate` cancels the running fetch instead of starting a second one. Quit cancels every fetch. Selections follow the row id across refreshes. One file per tab, `config_edit.go` for the Config edit flow, `config_scope.go` for its scope picker (`/`, type to filter by name, path, or `global`), `transcript.go` for the overlay.
- CLI children run in their own process group; cancel kills the group (`source/proc_unix.go`).
- `internal/fakesvc`, `internal/sandbox`, `internal/realsvc`: test helpers (fake socket, sandbox, prebuilt real service).
- `internal/fixture`: synthetic `Backend` scenarios for the preview command and screen tests.

## Checks

`just monitor-check` (from `apps`: `gofmt -l monitor`, `go vet ./monitor/...`, `go test -race ./monitor/...`). Commands below run from `apps/monitor`.

- Tests use fakes only: `fakesvc` sockets, fake `zdx` scripts, a fake `Backend`, a fake clock. Quota tests (`internal/source/quota_test.go`) pass a synthetic version, an `httptest` server for every endpoint, an empty `LookupEnv`, and a temp `oauth.json` through `source.Config.Quota`; never a real `oauth.json` or a live provider. Every test package calls `sandbox.Main` in `TestMain`: fresh `HOME`/`ZDX_HOME`/`TMPDIR`, other `ZDX_` variables and `OPENCODE_API_KEY` dropped except `ZDX_MONITOR_TEST_*`, a failing `zdx` first on PATH, and a failed run when the real home's `oauth.json`/`oauth.lock` or a live file in its `run/` directory changed (size and time only).
- Screen tests (`internal/ui/screens_test.go`): every screen of every fixture scenario at 60x16 to 200x50 must fill the height and stay within the width; `testdata/screens/*.txt` are text snapshots (time zone pinned to UTC). After an intended layout change: `ZDX_MONITOR_TEST_UPDATE=1 go test -run TestScreenSnapshots ./internal/ui`, then review the diff. `ZDX_MONITOR_TEST_DUMP_DIR=DIR go test -run TestDumpScreens ./internal/ui` writes every frame (`.ans` styled, `.txt` plain) for review.
- Check real terminal rendering, not only snapshots: run the preview command, or record it with VHS (`unset NO_COLOR` first).
- Create sockets under `os.MkdirTemp("", ...)` with short names: macOS limits socket paths to 104 bytes.
- Opt-in checks against a prebuilt binary (never built by the tests). Copy it first so a concurrent cargo build cannot replace it mid-run: `cp ../../target/debug/zdx "$TMPDIR/zdx-test" && ZDX_MONITOR_TEST_ZDX="$TMPDIR/zdx-test" go test -race -run TestReal ./internal/source ./internal/ui`.
  - `TestRealZdxShapes`: `zdx views`, `service status --json`, `jobs list --scheduler --json`, the `zdx helper` model picker, and a `config set-setting` dry run, write, and reset in a temp workspace, in the sandbox home with no service. It also needs `ZDX_MONITOR_TEST_GO`, a prebuilt `zdx-go` (`zdx jobs` execs it).
  - `TestRealServiceTranscript`: seeds thread JSONL (current `ThreadEvent` schema) and a run marker (`agent_activity::RunRecord`, the test's own PID) in a temp home, runs `zdx engine serve` there (env: `PATH`, `TMPDIR`, `LANG`, `LC_*`, then `HOME`/`ZDX_HOME`; no provider, no turn), and checks the full load, the alias `source_id`, `after` deltas, a `tool_running` row, the finished-but-unsaved row, and its completion (merge equals a full load at each step), the trajectory, and the UI model over that backend.
