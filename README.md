# ZDX

[![Ask DeepWiki](https://deepwiki.com/badge.svg)](https://deepwiki.com/tallesborges/zdx)
[![Built With Ratatui](https://ratatui.rs/built-with-ratatui/badge.svg)](https://ratatui.rs/)

ZDX CLI is a lightweight, multi-provider coding agent that runs locally on your computer,
works with your existing subscriptions (Claude, OpenAI/Codex, Gemini), and can optionally
connect to Telegram so you can chat anywhere.

![zdx demo](docs/assets/demo.gif)

> **Disclaimer**
>
> - **YOLO mode**: the agent can access your files and tools. It can make destructive changes. Use at your own risk.
> - **Early-stage**: expect breaking changes and unstable behavior while the project evolves.
> - **Unsigned binaries**: macOS builds are not code-signed or notarized yet, so Gatekeeper may block the app after install.

## Quickstart

### Install (macOS)

```bash
brew tap tallesborges/zdx
brew install --cask zdx
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

- **Providers**: Anthropic, OpenAI, OpenRouter, Moonshot, Gemini
- **Interactive TUI** with streaming markdown, syntax highlighting, and tables
- **Exec mode** for scripts and automation
- **Extended thinking** + token usage with pricing
- **Command palette**, **file picker** (`@`), **bash commands** (`$cmd`)
- **Session persistence** + timeline + thread switching
- **Project context** via `AGENTS.md` + **skills** via `SKILL.md`
- **Telegram bot** — interact via Telegram (`zdx bot`)

## Provider support

- **Anthropic**: API key + subscription (Claude CLI OAuth)
- **Gemini**: API key + subscription (Gemini CLI OAuth)
- **OpenAI**: API key + subscription (Codex OAuth)
- **Moonshot**: API key
- **OpenRouter**: API key

## Inspiration

This project was inspired by several excellent tools in the agentic coding space:

- [pi-mono](https://github.com/badlogic/pi-mono) — AI-powered coding agent with terminal UI and SDK for AI-assisted development
- [codex](https://github.com/openai/codex) — OpenAI's open-source terminal-based agentic coding assistant
- [AMP](https://ampcode.com/) — Great UX inspiration for agentic coding workflows
- [opencode](https://github.com/sst/opencode) — Open-source AI coding agent with TUI, desktop app, and VS Code extension

## License

MIT
