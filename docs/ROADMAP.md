# Roadmap

## Current Status (v0.1.0)

- [x] Basic exec and chat commands
- [x] Session persistence (JSONL format)
- [x] Session resume functionality
- [x] Configuration management
- [x] Claude API integration
- [x] Tool use (read files, bash commands)

## Planned

### Short-term

- [ ] Extended thinking support
- [ ] System prompt configuration
- [ ] AGENTS.md support (auto-include in context)
- [ ] `write` tool
- [ ] `edit` tool
- [ ] Basic diff preview for edit/write operations
- [ ] `exec --format json` (stable envelope for other CLIs)
- [ ] Streaming responses
- [ ] Session search/filter by content

### Medium-term

- [ ] Evaluate using `language_models` crate from Zed
- [ ] Multiple provider support (OpenAI, local models)
- [ ] Context file attachment (`--file <path>`)
- [ ] Project-aware context (auto-include relevant files)
- [ ] Custom system prompts

### Long-term

- [ ] Web UI for session browsing
