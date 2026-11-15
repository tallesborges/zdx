# apps/tui

The interactive ZDX chat in Go with Bubble Tea v2 (`zdx-go tui`). Part of the one Go module `apps/go.mod`, not part of the Cargo workspace: Rust builds never need Go, and the module never needs cargo. It imports `apps/shared` (socket transport, wire types, the transcript layout).

## Running it

- Bare `zdx` on a terminal execs `$ZDX_GO` (absolute) or `~/.local/bin/zdx-go` as `zdx-go tui --root ROOT [--thread ID] [--model SPEC]`, with `ZDX_HOME` and `ZDX_BIN` set (`crates/zdx-cli/src/cli/commands/chat.rs`, the `GoProgram` contract shared with `zdx bot` and `zdx monitor`). `zdx threads resume [ID]` passes that thread. Never a PATH lookup.
- `just run [ARGS]` builds `apps/bin/zdx-go` (without the Mini App) and runs `zdx` from this checkout with it; `just tui [ARGS]` runs `zdx-go tui` directly; `just go-install`, `just deploy`, and `just safe-deploy` install `~/.local/bin/zdx-go`.

## Boundaries

- Every turn runs in the orchestrator service (`$ZDX_HOME/run/orchestrator.sock`; `crates/zdx-engine/src/orchestration/protocol.rs` is the source of truth). The program runs no agent, provider, or tool, and reads or writes no thread, index, config, or credential file. It needs a running service (`zdx service start orchestrator` or `zdx orchestrator serve`) and says so at start.
- Events: one `subscribe` connection per shown thread (`thread_id` filter, capability `subscribe_thread`, never a renderer), opened before the first submit. The thread id is chosen here (a new UUID), so the subscription exists before the thread does. It carries every turn of the thread in order: ours, queued ones, a steer the turn did not read (it runs as the next turn), and turns other clients start. When it drops (service restart), the TUI resubscribes every 2 s and reloads the thread, since events may have passed.
- A message is a one-shot `submit` (`follow: "none"`) with `ordinary: true`, `chat: true` (capability `chat_submit`: the chat instruction layer, surface and activity kind `chat`), and the chat's `--root`. Enter while a turn runs sends `delivery: "steer"`; Tab sends `queue`. Esc or Ctrl+C sends `cancel`. Sent messages wait in an outbox (sending, queued, steer) until `turn_started` names their input id, `user_message_injected` reads the steer, or `inputs_dropped` drops it. The receipt and the subscription are separate connections, so either can come first (`early`).
- Live turns are kept per turn id and replaced by the saved items once the `thread_view` load asked after `turn_ended` comes back. Loads are numbered; an older answer never replaces a newer one.
- Model and thinking: `catalog` picker (scope `topic`) and `update_thread{model}` with the per-level spec the picker gives, as the bot does. A pick on a thread with no message yet waits and goes with the first submit (with the thread's `root`), so no empty thread is written. Status line: `catalog` `thread_status`. Title: `generate{title}` then `update_thread{title}` after the first message. Threads: `threads_list` (recent 50). Transcript: `thread_view`, merged with `after` cursors.
- Do not port engine rules here. Live tool rows show only what the events carry (name, outcome, the first short string argument, streamed output); the engine's tool views replace them once `thread_view` has the round.
- Never drop, merge, or reorder stream lines: `internal/engine` forwards every subscription event in order (no coalescing, unlike the gateway), and `internal/ui/live.go` appends every delta. `assistant_completed` replaces the streamed text of its message.
- Commands that read the service run off the event loop and return messages; only `Update` changes the model (pickers get their rows in a `pickerMsg`). Stopping a recording and transcribing also run off the loop. `Close` (after `Run`) kills a running recorder.
- Voice: `ffmpeg -f avfoundation -i :default` records a WAV (no audio code here), the service transcribes it (`transcribe_audio`). Images: a pasted image path or `/image PATH` goes through `prepare_image`. Copy: OSC 52 (`tea.SetClipboard`). `/login`, `/logout`: `zdx login|logout` through `tea.ExecProcess`. `/config`: `$VISUAL`/`$EDITOR` on `$ZDX_HOME/config.toml`.

## Layout

- `cli`: `zdx-go tui` (`Main`): flags (`--root`, `--zdx-home`, `--zdx`, `--thread`, `--model`), the capability check, `tea.NewProgram`.
- `internal/engine`: the chat's requests and the thread subscription over `apps/shared/socket`.
- `internal/ui`: `model.go` (state, messages, keys, turns), `live.go` (the running turn as transcript items), `commands.go` (pickers, slash commands, palette), `view.go` (layout), `picker.go`, `voice.go`. The transcript itself is `apps/shared/transcript`, the layout the monitor uses.

## Checks

`just tui-check` (from `apps`: `gofmt -l tui`, `go vet ./tui/...`, `go test -race ./tui/...`). Tests use fakes only (a fake socket in `internal/engine`, a fake backend in `internal/ui`); every package calls `apps/shared/sandbox.Main`. Create sockets under `os.MkdirTemp("", ...)`: macOS limits socket paths to 104 bytes.

Real check (manual): a sandbox `ZDX_HOME` with a config naming an API-key model, `zdx orchestrator serve` from this checkout's build in that home, then `zdx-go tui --root DIR` in a terminal or `tmux` (`capture-pane -p` to read the screen).
