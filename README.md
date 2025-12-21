# ZDX CLI

An agentic CLI tool powered by Claude for interactive coding assistance.

## Quickstart

### 1. Set up environment

```bash
export ANTHROPIC_API_KEY="your-api-key"
```

Optionally customize the config/data directory (default: `~/.config/zdx`):

```bash
export ZDX_HOME="$HOME/.zdx"
```

### 2. Initialize configuration (optional)

```bash
zdx config init
```

This creates a `config.toml` with defaults:

```toml
model = "claude-haiku-4-5"
max_tokens = 1024
tool_timeout_secs = 30

# system_prompt = "You are a helpful assistant."
# system_prompt_file = "/path/to/system_prompt.md"
```

### 3. Run commands

**One-shot execution:**

```bash
zdx exec -p "Explain what this Rust project does"
```

**Interactive chat:**

```bash
zdx
```

**Resume a previous session:**

```bash
# Resume the most recent session
zdx resume

# Resume a specific session
zdx resume abc123-session-id
```

## Session Storage

Sessions are stored as JSONL files in `$ZDX_HOME/sessions/` (default: `~/.config/zdx/sessions/`).

Each session file contains timestamped message events for the full conversation history.

### Managing sessions

```bash
# List all sessions (newest first)
zdx sessions list

# View a session transcript
zdx sessions show <session-id>
```

### Disabling session saving

Use the `--no-save` flag to run without persisting the session:

```bash
zdx exec -p "Quick question" --no-save
zdx chat --no-save
```

### Continuing an existing session

```bash
zdx exec -p "Follow-up question" --session <session-id>
zdx chat --session <session-id>
```

## CLI Reference

```
zdx <command>

Commands:
  exec       Execute a single prompt
  chat       Start interactive chat
  resume     Resume a previous session
  sessions   Manage saved sessions
  config     Manage configuration
```

### Common flags

| Flag | Description |
|------|-------------|
| `--session <ID>` | Continue an existing session |
| `--new-session` | Force creation of a new session |
| `--no-save` | Don't save the session |
| `--root <DIR>` | Set working directory for file operations |
| `--system-prompt <TEXT>` | Override system prompt (wins over config) |

## Project Context (AGENTS.md)

If an `AGENTS.md` file exists in the `--root` directory, its contents are appended to the effective system prompt automatically.

## Tool Timeout

Tool execution timeout is controlled by `tool_timeout_secs` in `config.toml` (default: `30`). Set `tool_timeout_secs = 0` to disable timeouts.

## Development

```bash
cargo build            # Debug build
cargo test             # Run tests
cargo clippy           # Lint
cargo fmt              # Format
```

## Docs

- `docs/SPEC.md` — values + contracts (source of truth)
- `docs/plans/plan_<short_slug>.md` — commit-sized implementation plans
- `docs/adr/` — Architecture Decision Records (the “why”)

## License

MIT
