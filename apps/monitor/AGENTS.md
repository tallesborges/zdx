# apps/monitor

Read-only ZDX dashboard in Go with Bubble Tea v2 (`charm.land/bubbletea/v2`, Lip Gloss v2). An independent Go module, not part of the Cargo workspace or `apps/gateway`. It does not replace the Rust monitor yet: `crates/zdx-monitor` and `zdx monitor` stay unchanged until this one is tested and the user approves.

## Running it

- Build: `go build -o "$TMPDIR/zdx-monitor" ./cmd/zdx-monitor` (from this directory). Not wired into `just`, CI, or `zdx monitor`.
- Layout preview without a home, service, or binary: `go run ./cmd/zdx-monitor-preview --scenario full|empty|down|loading|stale` (synthetic `internal/fixture` data).
- Run: `zdx-monitor [--root DIR] [--zdx-home DIR] [--zdx PATH] [--cli-fallback]`. Defaults: root `.`, home `$ZDX_HOME` else `~/.zdx`, binary `$ZDX_BIN` else `zdx` on PATH (a missing binary only breaks the tabs that need the CLI). Pass the orchestrator service's own `--root` (its `views_root`); the header warns on a mismatch, because config, usage, and job answers then belong to the service's root.
- It never starts a bot, an engine, or a service, and the monitor itself writes no ZDX state. Reading is not free of disk writes: the engine views it calls may bring derived caches (thread index, usage cache) up to date, and the service drops stale run markers.

## Boundaries

- Rust owns every rule: config resolution, usage accounting, worker and run state, job status, indexed thread queries, service control. Go decodes and lays out what Rust returns. Do not port engine rules here; ask for a socket view (with its `zdx views` twin) instead.
- Views come from the orchestrator socket (`$ZDX_HOME/run/orchestrator.sock`, NDJSON, one request per connection; source of truth `crates/zdx-engine/src/orchestration/protocol.rs` and `views.rs`). The client checks the view capability with a `status` call before every view, as the gateway does. It sends only `status` and view ops.
- `--cli-fallback` runs views through `zdx --root ROOT views --file -` while the socket has no listener. Off by default: an in-process view refreshes derived caches (thread index, usage cache) in that process. The header says when it is active.
- Read-only CLI JSON: `zdx service status --json` (service state without the orchestrator; reads pidfiles and launchd) and `zdx jobs list --daemon --json` (`job_daemon::jobs()`, the Rust monitor's Jobs source). Children get `ZDX_HOME` set to the monitor's home.
- Logs: the tail (at most 1 MiB, then the last 2000 lines) of `$ZDX_HOME/logs/zdx.log*`, as the Rust monitor reads them. No view exists for them; no other directory is listed.
- No directory scans for corpus data and no fallback that invents data. Missing data stays unknown, stale, or an error on screen.
- Do not import `apps/gateway/internal/...`. A shared Go client package needs coordination first.

## Surfaces and gaps

| Tab | Source | Missing vs the Rust monitor |
| --- | --- | --- |
| Agents | `active_agents` as a tree + `thread_view` transcript (cursor polls, merged as `apps/web` merges) + `i` overlay over `thread_trajectory` | thinking toggles all blocks, not one; no mouse click on a tool; no expand/collapse |
| Threads | `threads_list` (recent 50, `all`/`workers`) | indexed browse (500 rows, kind/project/text filters, project picker); `o` open JSONL |
| Services | `zdx service status --json` + socket `status` threads | controls (not built; CLI exists, see below) |
| Jobs | `zdx jobs list --daemon --json` | model edit, archived list (CLI exists) |
| Usage | `usage_report` (`[`/`]` days) + `subscription_quotas` (on entry, `Q`, every 5 min) | charts are one sparkline |
| Logs | rolling log files | target filter picker, copy, line overlay |
| Background | `monitor_snapshot.background_processes` | detail log tails; kill (CLI exists) |
| Config | `workspaces` + `monitor_snapshot.config`, read only | every edit, reach badges, override counts, confirmations |

- Agents tree (`agent_tree.go`): a run nests under the running run whose `thread_id` equals its marker's `parent_thread_id` (a worker's owner, a subagent's or helper's caller). A parent with no run is a dim context row (not counted, `↵` opens its thread); links that would close a cycle are dropped; siblings keep the engine's order. No nesting is inferred from role, model, root, or order. The MODEL column is one spec, `provider[@account]:model@thinking`, built from the marker's `provider`, `account`, `model`, and `thinking` (`specParts`). A model that starts with `provider:` or `provider@account:` keeps its own prefix; the level is not added again when the model already has it as an `@level` suffix. Any other `@` or `:` in a model id is opaque, and a missing part is left out. Narrow columns drop the account, then cut the provider at the provider boundary before the `model@thinking` end (`shortSpec`); the detail pane shows the full spec and each part.
- List columns show one unit of an engine duration (`compactAge`: `1h 23m` → `1h`) so a column never cuts a duration; details show the full value.
- Transcript merge: a delta whose cursor went back (thread reset) or whose `source_id` changed (alias moved) is dropped and the thread loads in full again. A `tool_running` item with `completed_at` is a finished call whose round is not saved yet; it shows its `ok` outcome (unknown when absent), never "running". Message attachments (`artifacts`) show as name, kind, and path.
- External text is untrusted: `applyResult` runs `cleanAll` (`sanitize.go`) on every answer, and `oneLine` cleans error strings, so escape sequences (SGR, OSC 8, DCS/APC passthrough), C0/C1 controls, and carriage returns never reach the renderer. Bubble Tea's renderer applies SGR and hyperlinks and forwards terminated DCS/APC/SOS/PM sequences, so do not rely on it.
- `render` never returns a frame larger than the terminal; below 20x6 it shows a short "terminal too small" frame.
- `thread_trajectory` does not follow an alias (`thread_view` does, and returns `source_id`). The overlay asks for the transcript's `source_id`, so it waits for the first `thread_view` answer.

Existing engine interfaces for the missing parts (reuse them when controls are approved; none is wired yet):

- Service start/stop/restart: `zdx service start|stop|restart [SERVICE]`. Stop and restart refuse active agent runs and in-flight orchestrator work unless `--force` (the Rust monitor's `r`/`R`); `restart --queue` queues it.
- Background kill: `zdx bg kill <bg_id>`. Exited processes: `zdx bg list --json`.
- Archived once jobs: `zdx jobs list --archived --json`.
- Job model edit: `zdx jobs set-model NAME SPEC|--clear`. Missing: the compare-and-set guard the Rust monitor passes (`jobs::set_model` `expected`); the CLI passes `None`.
- Model picker list: `zdx models list --json`.

Genuinely missing (each needs a JSON CLI or view in Rust):

- Indexed thread browse: `thread_index::browse_threads` (kind, project, text, 500 rows) and `browse_projects` have no CLI or view. `zdx threads search --json` is FTS plus dates only; `threads_list` is the 50 most recent.
- Background detail: `bg list --json` and `monitor_snapshot` carry no stdout/stderr log paths or tails (`background_activity::stdout_log_path`, `read_log_tail`).
- Config writes: `workspace_config::set_setting`/`reset_setting` on a chosen layer file, with `reach` and `own_override_count`, have no CLI. `zdx config set|unset [--local]` writes raw keys to the global file or the current directory's workspace only, without the setting model or validation.

## Layout

- `cmd/zdx-monitor`: flags and `tea.NewProgram` with a cancellable context.
- `internal/wire`: socket client (`Status`, `View`, capability check, reply budgets, cancel closes the connection).
- `internal/data`: JSON body types, decode only.
- `internal/source`: the production backend: socket views, optional CLI fallback, CLI JSON, log tail.
- `internal/ui`: Bubble Tea model. `chrome.go`: header (connection and warnings only; paths, protocol, and capabilities are in the `?` help), tab bar (fits by dropping badges, then long labels), footer (hints that fit, freshness, a line per stale surface), help. `layout.go`: the shared `table` (auto-sized and flex columns, `minWidth` hiding, `cutLeft`, selection background that keeps cell colors), `listWithDetail`, duration and time formatting. `fetch.go` is the refresh loop: one fetch per key at a time, a generation per key so an answer for an older request (other thread, kind, days, log file) is dropped, and `invalidate` cancels the running fetch instead of starting a second one. Quit cancels every fetch. Selections follow the row id across refreshes. One file per tab, `transcript.go` for the overlay.
- CLI children run in their own process group; cancel kills the group (`source/proc_unix.go`).
- `internal/fakesvc`, `internal/sandbox`, `internal/realsvc`: test helpers (fake socket, sandbox, prebuilt real service).
- `internal/fixture`: synthetic `Backend` scenarios for the preview command and screen tests.

## Checks

From `apps/monitor`: `gofmt -l .`, `go vet ./...`, `go test -race ./...`.

- Tests use fakes only: `fakesvc` sockets, fake `zdx` scripts, a fake `Backend`, a fake clock. Every test package calls `sandbox.Main` in `TestMain`: fresh `HOME`/`ZDX_HOME`/`TMPDIR`, other `ZDX_` variables dropped except `ZDX_MONITOR_TEST_*`, a failing `zdx` first on PATH, and a failed run when a live file in the real `run/` directory changed.
- Screen tests (`internal/ui/screens_test.go`): every screen of every fixture scenario at 60x16 to 200x50 must fill the height and stay within the width; `testdata/screens/*.txt` are text snapshots (time zone pinned to UTC). After an intended layout change: `ZDX_MONITOR_TEST_UPDATE=1 go test -run TestScreenSnapshots ./internal/ui`, then review the diff. `ZDX_MONITOR_TEST_DUMP_DIR=DIR go test -run TestDumpScreens ./internal/ui` writes every frame (`.ans` styled, `.txt` plain) for review.
- Check real terminal rendering, not only snapshots: run the preview command, or record it with VHS (`unset NO_COLOR` first).
- Create sockets under `os.MkdirTemp("", ...)` with short names: macOS limits socket paths to 104 bytes.
- Opt-in checks against a prebuilt binary (never built by the tests). Copy it first so a concurrent cargo build cannot replace it mid-run: `cp ../../target/debug/zdx "$TMPDIR/zdx-test" && ZDX_MONITOR_TEST_ZDX="$TMPDIR/zdx-test" go test -race -run TestReal ./internal/source ./internal/ui`.
  - `TestRealZdxShapes`: `zdx views`, `service status --json`, `jobs list --daemon --json` in the sandbox home with no service.
  - `TestRealServiceTranscript`: seeds thread JSONL (current `ThreadEvent` schema) and a run marker (`agent_activity::RunRecord`, the test's own PID) in a temp home, runs `zdx orchestrator serve` there (env: `PATH`, `TMPDIR`, `LANG`, `LC_*`, then `HOME`/`ZDX_HOME`; no provider, no turn), and checks the full load, the alias `source_id`, `after` deltas, a `tool_running` row, the finished-but-unsaved row, and its completion (merge equals a full load at each step), the trajectory, and the UI model over that backend.
