# ZDX

An agentic TUI powered by Claude for interactive coding assistance.

![zdx demo](docs/assets/demo.gif)

## Why this exists

This is a **personal learning project**, built for fun and curiosity. The goal is to explore how agentic coding tools work by building one from scratch.

It's not designed for public or general usage — it's focused on my own needs and what I think makes a good coding assistant. If you find it useful, great! But expect opinionated choices and features that match my workflow rather than broad compatibility.

## Features

- **Tools:** bash, read (files + images), edit, write
- **Streaming markdown** with syntax highlighting and table support
- **Extended thinking** with configurable levels and block display
- **Command palette** overlay (Ctrl+P or `/`) — model picker, thinking level, and more
- **Text selection** — click and drag to select, auto-copies to clipboard
- **Token usage** display with pricing
- **Session persistence** — resume any previous conversation
- **Handoff** — AMP-style context-aware prompt generation to start fresh sessions
- **Project context** via `AGENTS.md` files (recursively loaded from parent directories)

## Inspiration

This project was inspired by several excellent tools in the agentic coding space:

- [pi-mono](https://github.com/badlogic/pi-mono) — AI-powered coding agent with terminal UI and SDK for AI-assisted development
- [codex](https://github.com/openai/codex) — OpenAI's open-source terminal-based agentic coding assistant
- [AMP](https://ampcode.com/) — Great UX inspiration for agentic coding workflows
- [opencode](https://github.com/sst/opencode) — Open-source AI coding agent with TUI, desktop app, and VS Code extension

## License

MIT
