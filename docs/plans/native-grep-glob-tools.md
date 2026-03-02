# Native Grep & Glob Tools

## Goals
- Add a native `Grep` tool to ZDX that returns structured JSON results (file, line, match, context)
- Add a native `Glob` tool for file discovery by name pattern
- Eliminate dependency on external `rg` binary for search use cases
- Save tokens by giving the agent typed output instead of raw text to parse

## Non-goals
- Replacing bash tool (bash stays for general commands)
- Full ripgrep CLI feature parity
- In-memory search (search string content, not files)
- FS cache (overkill for personal use)
- Parallel search via rayon (premature; single-threaded is fast enough)

## Design principles
- User journey drives order
- Structured output over raw text
- No new external binary dependencies (embed via crates)

## User journey
1. Agent needs to search for a pattern in the codebase
2. Agent calls `Grep` tool with `pattern`, optional `path`, optional `glob`, optional `context_lines`
3. Tool returns structured JSON: array of `{file, line_number, col, text, context_before, context_after}`
4. Agent processes typed results, no parsing needed
5. Agent needs to find files by name — calls `Glob` tool with pattern
6. Tool returns structured list of matching file paths

## Foundations / Already shipped (✅)

### Bash tool
- What exists: `bash.rs` — shells out to `sh -c <command>`, agent uses `rg` via bash
- ✅ Demo: `just run` → ask agent to search for a pattern → it calls Bash with rg
- Gaps: raw text output, requires rg installed, no structured data

### Tools infrastructure
- What exists: `src/tools/mod.rs` with `ToolDefinition`, `ToolContext`, `ToolOutput` pattern
- ✅ Demo: all existing tools follow the same pattern (read, write, bash, etc.)
- Gaps: none — just add a new file

## MVP slices (ship-shaped, demoable)

### Slice 1: Basic grep tool (pattern + path)
- **Goal**: Working grep tool with minimum viable params — pattern and optional path
- **Scope checklist**:
  - [ ] Add `crates/zdx-core/Cargo.toml`: `grep-regex`, `grep-searcher`, `grep-matcher` (ripgrep internals)
  - [ ] Create `src/tools/grep.rs` with `definition()` and `execute()`
  - [ ] Input schema: `pattern` (required), `path` (optional, defaults to root), `case_insensitive` (optional bool)
  - [ ] Output: `{ matches: [{file, line_number, text}], total_matches, truncated }`
  - [ ] Cap results at 200 matches (prevent context flooding)
  - [ ] Skip files > 4MB
  - [ ] Brace sanitization: auto-escape `${var}` in patterns
  - [ ] Register in `src/tools/mod.rs`
  - [ ] Update `crates/zdx-core/AGENTS.md`
- **✅ Demo**: Ask agent "find all uses of ToolOutput in zdx-core" → agent calls Grep → gets structured JSON with file paths and line numbers
- **Risks / failure modes**:
  - ripgrep crates API surface — check `grep-searcher` docs for correct builder usage
  - Binary files: ensure they're skipped (ripgrep does this by default)

### Slice 2: Glob filtering + context lines
- **Goal**: Make grep useful for scoped searches and code review
- **Scope checklist**:
  - [ ] Add `glob` param (e.g. `"*.rs"`, `"src/**/*.ts"`) using `globset` crate
  - [ ] Add `context_lines` param (0–5, default 0) — returns lines before/after match
  - [ ] Output: `{ matches: [{file, line_number, col, text, context_before[], context_after[]}] }`
  - [ ] Add `.gitignore` respect via `ignore` crate (WalkBuilder)
  - [ ] Column truncation: truncate lines beyond 500 chars (match zdx Read tool's MAX_LINE_LENGTH)
  - [ ] Round-robin match selection across files (prevents all results from one file)
- **✅ Demo**: Ask agent to "find all TODO comments in Rust files" → Grep called with `glob: "*.rs"` → clean typed list
- **Risks / failure modes**:
  - `ignore` crate integration — needs WalkBuilder configured with root path

### Slice 3: Glob tool (file name search)
- **Goal**: Let agent find files by name pattern (replaces `rg --files -g`, `find . -name`)
- **Scope checklist**:
  - [ ] Create `src/tools/glob.rs` with `definition()` and `execute()`
  - [ ] Uses `ignore::WalkBuilder` + `globset` to walk + filter by filename pattern
  - [ ] Input schema: `pattern` (required, e.g. `"*.rs"`, `"**/AGENTS.md"`), `path` (optional)
  - [ ] Returns `{ files: [string], total, truncated }` — flat list of matching paths
  - [ ] Respects `.gitignore` by default; retry without gitignore if 0 results
  - [ ] Auto-recursive: `"*.rs"` → `"**/*.rs"`
  - [ ] Cap at 500 files
  - [ ] Sort results alphabetically
  - [ ] Register in `src/tools/mod.rs`
  - [ ] Update `crates/zdx-core/AGENTS.md`
- **✅ Demo**: Ask agent "find all AGENTS.md files in the repo" → structured list, no bash needed
- **Risks / failure modes**:
  - Large repos: ensure walk is bounded by result cap + timeout

## Contracts (guardrails)
- Grep/Glob tools must NOT break existing Bash tool behavior
- Results must always be valid JSON (no panics on malformed input or binary files)
- Grep result cap at 200 matches, Glob cap at 500 files (hardcoded consts, not user params)
- `.gitignore` must be respected by default in Slice 2+
- Skip files > 4MB

## Key decisions (decide early)
- **Crate choice**: use `grep-regex` + `grep-searcher` + `grep-matcher` (ripgrep internals). Alternative: `regex` crate + manual file walking. Decision: use ripgrep internals for correctness + performance.
- **Separate tools**: Grep and Glob are separate tools (different schema, different use case)
- **Result caps**: 200 matches (grep), 500 files (glob) — hardcoded constants
- **Structured output**: JSON objects, NOT plain text — whole point is structured data
- **No parallel search**: single-threaded for now (premature for personal use)
- **No FS cache**: overkill for personal use

## Testing
- Manual smoke demos per slice (run zdx, ask it to search)
- Unit test: known fixture directory → expected matches count + file/line
- Unit test: binary file skipped
- Unit test: .gitignore respected
- Unit test: brace sanitization (`${var}` pattern doesn't error)
- Unit test: auto-recursive glob prefix

## Polish phases (after MVP)

### Phase 1: Multiline + regex flags
- Full regex syntax docs in tool description
- `max_matches` param (override cap per call)
- `offset` param for pagination
- ✅ Check-in demo: complex regex search works reliably

### Phase 2: Performance metrics
- Log search duration for large repos
- ✅ Check-in demo: search zdx repo in <100ms

## Later / Deferred
- Parallel search via rayon — premature optimization; single-threaded is fast enough
- FS scan cache — overkill for personal use; revisit if perf is an issue
- Fuzzy file matching — not needed yet, glob patterns cover the use case
- Sort by mtime (glob) — nice-to-have, not MVP
- Streaming on_match callback — useful for TUI progress, but not needed for v1
