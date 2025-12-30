# Navigation

- CLI flags & subcommands: `src/app/mod.rs`, `src/app/commands/`
- Exec mode output formatting: `src/ui/exec.rs`
- TUI runtime/event loop: `src/ui/chat/mod.rs`
- TUI keybinds & state changes: `src/ui/chat/update.rs`
- TUI layout/colors: `src/ui/chat/view.rs`
- Session log format & replay: `src/core/session.rs`
- Agent loop (tool + provider orchestration): `src/core/agent.rs`
- Anthropic streaming parsing: `src/providers/anthropic/sse.rs`
- Tool schemas/registry: `src/tools/mod.rs`
