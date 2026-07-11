# zdx

![Zdx Hero](docs/assets/hero.png)

[![Ask DeepWiki](https://deepwiki.com/badge.svg)](https://deepwiki.com/tallesborges/zdx)
[![Built With Ratatui](https://ratatui.rs/built-with-ratatui/badge.svg)](https://ratatui.rs/)

Zdx is a coding agent built in Rust. I made it to have customization freedom and also for learnig purpose.

![Zdx demo](docs/assets/demo.gif)

> **Disclaimer**
>
> - **YOLO mode**: the agent can access your files and tools. It can make destructive changes. Use at your own risk.
> - **Early-stage**: expect breaking changes and unstable behavior while the project evolves.

## Quickstart

### Install

**From source:**
```bash
git clone https://github.com/tallesborges/zdx.git
cd zdx
cargo install --path crates/zdx-cli
```

### Run

```bash
zdx --help
zdx
```

### Exec mode

```bash
zdx exec -p "hello"
```

### Telegram bot

```bash
zdx bot
```

> First time? Run `zdx config init` and set your provider + telegram keys.

## Features

Just the ones I use — but that turns out to be a lot.

### Interfaces

- **Interactive TUI** — the main terminal experience (built with Ratatui)
- **Exec mode** — headless, scriptable one-shot runs (`zdx exec -p "..."`)
- **Telegram bot** — drive the agent from your phone
- **Monitor dashboard** — a separate TUI to watch agent runs, services, logs, and config

### Providers & models

- **API providers** — Anthropic, OpenAI, Google Gemini, xAI (Grok), DeepSeek, Moonshot (Kimi), MiniMax, Mistral, StepFun, Z.ai (GLM), Xiaomi MiMo, Meta (Muse Spark)
- **Subscription / CLI auth** — Claude CLI, Gemini CLI, OpenAI Codex, Google Antigravity, Xiaomi Token Plan
- **Aggregators** — OpenRouter, opencode-go
- **Local** — LM Studio
- **Custom** — any OpenAI-compatible provider defined straight from config
- **OpenAI Responses transport** (opt-in) + **priority fast mode** (`/fast`)
- Model registry with pricing, context limits, and reasoning detection

### Built-in tools

- **Filesystem & shell** — `read`, `write`, `edit`, `apply_patch`, `bash`, `glob`, `grep`
- **Web** — `web_search`, `fetch_webpage`
- **Agent** — `todo_write` (task tracking), `invoke_subagent`, `memory_search`, `memory_get`, `thread_search`, `read_thread`

### Subagents

Delegate scoped work to isolated child agents:

- **`task`** — general-purpose delegated execution (default)
- **`explorer`** — read-only, multi-round local/repo/thread/doc exploration (has read-only `bash`)
- **`oracle`** — deep-reasoning advisor for review, debugging, planning, and architecture

### Skills & commands

- **Bundled skills** — `brainstorm`, `define-goal`, `deep-research`, `imagine` (image gen/edit), `memory`, `ship-first-plan`, `skill-creator`
- **Your own skills** — project and user `SKILL.md` files, auto-discovered per scope
- **Bundled commands** — `plan`, `execute-plan`, `investigate`, `review-loop`, `simplify`, `skillify`, `commandify`, `skillpack`, `self-improve`, `init-agents-md`, `onboarding`
- **Custom Markdown slash commands**, fuzzy-matched in the command palette

### Memory & threads

- **qmd-backed memory** — hybrid/vector/keyword search across notes, calendar, and past threads
- **Threads** — resume, rename, append, export, search, and tool-call inspection; open as tabs and forks

### TUI niceties

- Tabbed threads/forks, image attachments via the **Kitty graphics protocol**, **voice dictation**
- Overlays for context usage, thread TLDR, and skills; model picker with Tab-cycled favorites
- Inline questions + follow-up suggestions, `/prompt-builder`, and a `btw` side chat

### Telegram bot

- Interactive questions & follow-up buttons, per-project chat routing
- `/model` override, thinking controls, and file/document attachments

### Automations & MCP

- **Automations** — scheduled, headless agent jobs (reports, reminders, actions)
- **MCP** — connect remote MCP servers (with OAuth), plus schema/call helpers

### Context & config

- Ancestor-scoped **`AGENTS.md`/`CLAUDE.md`** and **`.zdx/skills`** discovery
- TOML config, provider login/logout, git **worktree** management, and thread-scoped artifact dirs

## Inspiration

This project was inspired by several excellent tools in the agentic coding space:

- [pi-mono](https://github.com/badlogic/pi-mono) — AI-powered coding agent with terminal UI and SDK for AI-assisted development
- [codex](https://github.com/openai/codex) — OpenAI's open-source terminal-based agentic coding assistant
- [AMP](https://ampcode.com/) — Great UX inspiration for agentic coding workflows
- [opencode](https://github.com/sst/opencode) — Open-source AI coding agent with TUI, desktop app, and VS Code extension
- oh-my-pi
## License

MIT
