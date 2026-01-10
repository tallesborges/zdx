# ZDX

A fast, beautiful agentic coding assistant.

![zdx demo](docs/assets/demo.gif)

## What it is

ZDX is a TUI coding agent that helps you as a developer be productive. The vision is to be a faster, lightweight coding agent.

## Why it exists

Built because I needed something:
- **Productive** — shortcuts, command palette, everything at my fingertips
- **Fast and lightweight** — can spawn multiple instances when needed
- **Pleasant to use** — beautiful and elegant, for all-day use

## Features

- **4 tools:** `bash`, `read` (files + images), `edit`, `write` — all you need for real work
- **Anthropic provider** with API key or OAuth authentication (Claude Pro/Max)
- **OpenAI provider** with API key (OpenAI API) or OAuth (Codex Subscription)
- **OpenRouter provider** with API key
- **Gemini provider** with API key
- **Gemini CLI provider** with OAuth (Cloud Code Assist)
- **Interactive TUI** with streaming markdown, syntax highlighting, and table support
- **Exec mode** — non-interactive mode for scripts and automation
- **Extended thinking** with configurable levels and block display
- **Command palette** overlay (Ctrl+P or `/`) — model picker, thinking level, and more
- **File picker** — type `@` to browse and insert project files (respects .gitignore)
- **Bang commands** — type `!<command>` to execute bash commands directly (e.g., `!ls -la`, `!git status`)
- **Text selection** — click and drag to select, double-click selects a word, auto-copies to clipboard
- **Token usage** display with pricing
- **Session persistence** — resume or switch between previous threads (filtered by workspace, Ctrl+T to show all)
- **Timeline overlay** — jump to turns and fork from any message
- **Project context** via `AGENTS.md` files (recursively loaded from parent directories)

## Inspiration

This project was inspired by several excellent tools in the agentic coding space:

- [pi-mono](https://github.com/badlogic/pi-mono) — AI-powered coding agent with terminal UI and SDK for AI-assisted development
- [codex](https://github.com/openai/codex) — OpenAI's open-source terminal-based agentic coding assistant
- [AMP](https://ampcode.com/) — Great UX inspiration for agentic coding workflows
- [opencode](https://github.com/sst/opencode) — Open-source AI coding agent with TUI, desktop app, and VS Code extension

## License

MIT
