# Zdx

[![Ask DeepWiki](https://deepwiki.com/badge.svg)](https://deepwiki.com/tallesborges/zdx)
[![Built With Ratatui](https://ratatui.rs/built-with-ratatui/badge.svg)](https://ratatui.rs/)

Zdx is a coding agent built in Rust. I made it mainly to avoid using multiple CLIs and to have customization freedom, so I try to add the best features from them into a single CLI.

I added the Telegram bot so I can control it while away from the computer. I don't have plans to add providers other than Telegram.

![Zdx demo](docs/assets/demo.gif)

> **Disclaimer**
>
> - **YOLO mode**: the agent can access your files and tools. It can make destructive changes. Use at your own risk.
> - **Early-stage**: expect breaking changes and unstable behavior while the project evolves.

## Quickstart

### Install (macOS)

```bash
brew tap tallesborges/zdx
brew install zdx
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

- **Providers**: Anthropic, Gemini, OpenAI, OpenRouter, Mistral, Moonshot, MiMo, StepFun
- **Interactive TUI** with streaming markdown, syntax highlighting, and tables
- **Exec mode** for scripts and automation
- **Extended thinking** + token usage with pricing
- **Command palette**, **file picker** (`@`), **bash commands** (`$cmd`)
- **Session persistence** + timeline + thread switching
- **Project context** via `AGENTS.md` + **skills** via `SKILL.md`
- **Telegram bot** — interact via Telegram (`zdx bot`)

## Provider support

- **Anthropic**: API key + OAuth (Claude CLI, subscription)
- **Gemini**: API key + OAuth (Gemini CLI, subscription)
- **OpenAI**: API key + OAuth (Codex, subscription)
- **OpenRouter**: API key
- **Mistral**: API key
- **Moonshot (Kimi)**: API key
- **MiMo (Xiaomi)**: API key
- **StepFun**: API key

## Inspiration

This project was inspired by several excellent tools in the agentic coding space:

- [pi-mono](https://github.com/badlogic/pi-mono) — AI-powered coding agent with terminal UI and SDK for AI-assisted development
- [codex](https://github.com/openai/codex) — OpenAI's open-source terminal-based agentic coding assistant
- [AMP](https://ampcode.com/) — Great UX inspiration for agentic coding workflows
- [opencode](https://github.com/sst/opencode) — Open-source AI coding agent with TUI, desktop app, and VS Code extension

## License

MIT
