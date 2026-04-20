# Tools Restriction

## Goals
- Allow restricting which tools are available when running handoff generation
- Prevent destructive changes during handoff by limiting to read-only operations
- Provide CLI flag to control tool access in exec mode (usable by handoff and direct exec)

## Non-goals
- Per-tool fine-grained permissions beyond simple inclusion/exclusion
- Runtime tool approval prompts during handoff
- Tool restrictions for interactive TUI mode (out of scope for this feature)

## Design principles
- User journey drives order
- Ship-first: minimal CLI flag, then wire through handoff
- `--tools` flag is source of truth when passed (full override, not intersection)
- Keep tests minimal (KISS) - only protect user-visible contracts

## User journey
1. User triggers handoff with `/handoff <goal>` in TUI
2. Handoff subagent spawns with restricted tools (e.g., only `read`)
3. Subagent generates handoff prompt using only allowed tools
4. User receives handoff prompt without risk of file modifications

## Foundations / Already shipped (✅)

### Tool filtering infrastructure
- What exists: `ProviderConfig::filter_tools()` filters tools by explicit list
- ✅ Demo: `cargo test test_filter_tools_explicit_include`
- Gaps: None - this is the mechanism we'll use

### Exec mode
- What exists: `zdx exec -p <prompt>` runs non-interactive agent turn
- ✅ Demo: `cargo run -p zdx -- exec -p "hello"`
- Gaps: No `--tools` flag to override provider defaults

### Handoff subagent spawning
- What exists: `handoff.rs::run_subagent()` spawns `zdx --no-thread exec` as subprocess
- ✅ Demo: `/handoff test` in TUI (requires active thread)
- Gaps: Hardcoded args, no tool restriction

## MVP slices (ship-shaped, demoable)

## Slice 1: Add `--tools` CLI flag to exec command
- Goal: Allow `zdx exec --tools read,bash -p "..."` to restrict available tools
- Scope checklist:
  - [ ] Add `--tools` arg to `Commands::Exec` in `src/cli/mod.rs`
  - [ ] Pass tools list through `commands::exec::run()` to agent
  - [ ] When `--tools` is provided, use it as the enabled tools set (full override)
  - [ ] Add `tools_override: Option<Vec<String>>` to `ExecOptions`
- ✅ Demo:
  - `zdx exec --tools read -p "list files in current dir"` → agent can only use `read` tool
  - `zdx exec --tools read -p "create a file"` → agent attempts but `write` tool unavailable
- Risks / failure modes:
  - User typos tool names → fail gracefully with "unknown tool" error from existing validation

## Slice 2: Wire tool restriction through handoff
- Goal: Handoff subagent uses only `read` tool by default
- Scope checklist:
  - [ ] Update `run_subagent()` in `zdx-tui/src/runtime/handoff.rs` to pass `--tools read` in args
  - Note: This is already in the runtime handler layer (correct location per architecture)
- ✅ Demo:
  - `/handoff summarize this thread` → subagent runs with only `read` tool
  - Check stderr output shows only `read` tool available
- Risks / failure modes:
  - Handoff prompt might instruct agent to use tools it doesn't have → prompt template already focuses on summarization

## Contracts (guardrails)
- Existing tool execution validation must still apply (unknown tools rejected)
- `--tools` flag with empty value should error, not disable all tools
- `--tools` is a full override when provided (ignores provider defaults)

## Key decisions (decide early)
- **Flag format**: `--tools read,bash` (comma-separated) vs `--tools read --tools bash` (repeated)
  - Decision: comma-separated for simplicity and subprocess compatibility
- **Default handoff tools**: Should be `read` only (safest) or configurable?
  - Decision: Default to `read` only; can add config later if needed

## Testing
- Manual smoke demos per slice (see ✅ Demo sections above)
- No additional unit tests unless a regression occurs

## Polish phases (after MVP)

### Phase 1: Error UX
- Better error message when restricted tool is requested
- Suggest available tools in error output
- ✅ Check-in: Error message is actionable

## Later / Deferred
- **Configurable handoff tools**: Add `handoff_tools` to `config.toml`
  - Trigger: User needs different tools for handoff generation
- **Interactive tool restrictions**: Applying restrictions to TUI mode
  - Trigger: User request for "safe mode" in interactive
- **Tool approval prompts**: Runtime confirmation before executing tools
  - Trigger: Security audit or user request
- **Per-session tool config**: Override tools via environment variable
  - Trigger: Integration with external automation
