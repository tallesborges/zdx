# Output Truncation Plan

## Inputs

- **Project/feature**: Safe truncation of tool outputs sent to the AI to prevent context window overflow. Start with Read tool (line limits + line-level truncation like codex-rs), later extend to Bash outputs with a temp file mechanism so the AI can request more data if needed.
- **Existing state**: 
  - Read tool has simple byte truncation at 50KB (`MAX_TEXT_BYTES`), cuts mid-content
  - Bash tool has no truncation - returns full stdout/stderr
  - `ToolOutput` envelope supports `truncated` flag in data
- **Constraints**: Must not break existing tool output format; truncation should be transparent to the AI (it should know when content is truncated and how to request more)
- **Success looks like**: AI receives manageable chunks of file/command output with clear truncation metadata and can request additional content when needed

---

# Goals

- Prevent context window overflow from large file reads
- Provide line-aware truncation (preserve whole lines, truncate long lines silently)
- Give the AI clear signals when file is truncated via metadata fields
- (Later) Allow AI to request more data from truncated outputs via temp file references

# Non-goals

- Token-based truncation (requires tokenizer integration)
- Configurable limits per model (use sensible defaults first)
- Head+tail preservation for Read (simpler: just head with offset support)
- Real-time streaming truncation for Bash (defer to later phase)
- Exposing line-level char truncation to AI (silent truncation, AI uses Bash for edge cases)

# Design principles

- **User journey drives order**: Read truncation first (most common large-output case)
- **Ship-first**: Simple line-based truncation before fancy features
- **Transparent truncation**: AI always knows when file is truncated (not char-level)
- **Pure content**: Keep `content` field as faithful file slice - no synthetic markers mixed in
- **Simple code over performance**: Always scan for total_lines (one code path, easier to maintain)
- **YAGNI**: Don't add fields until needed (no `bytes`, no `line_truncation_count`)

# User journey

1. User asks AI to read a large file
2. AI calls Read tool with file path
3. Read tool returns first N lines, long lines silently truncated at M chars
4. Output includes truncation metadata (lines shown, total lines)
5. AI sees truncation info and can request a different offset if needed
6. (Later) User asks AI to run a command with large output
7. AI calls Bash tool
8. Bash returns truncated output + temp file path
9. AI can call Read on temp file with offset to get more

---

# Foundations / Already shipped (✅)

## Read tool basic truncation
- **What exists**: 50KB byte-based truncation with `truncated: true` flag
- ✅ Demo: `cargo test -p zdx-core test_read_large_file_truncated`
- **Gaps**: Cuts mid-line, no line count info, no offset parameter

## Bash tool execution
- **What exists**: Full stdout/stderr capture, timeout support
- ✅ Demo: `cargo test -p zdx-core test_bash_executes_command`
- **Gaps**: No truncation, no temp file storage

## ToolOutput envelope
- **What exists**: Structured `{ok, data}` / `{ok, error}` format with JSON serialization
- ✅ Demo: `cargo test -p zdx-core test_tool_output_success_roundtrip`
- **Gaps**: None for this feature

---

# MVP slices (ship-shaped, demoable)

## Slice 1: Read tool - line-based truncation

- **Goal**: Replace byte truncation with line-aware truncation (like codex-rs)
- **Scope checklist**:
  - [ ] Add constants: `MAX_LINES = 2000`, `MAX_LINE_LENGTH = 500`
  - [ ] Implement `read_text_lines()` that reads line-by-line with limits
  - [ ] Use hard byte limit per line to prevent memory issues with huge single-line files
  - [ ] Truncate individual lines exceeding `MAX_LINE_LENGTH` at char boundary (silent, no marker)
  - [ ] Always scan entire file for `total_lines` (simpler code, one path)
  - [ ] Return structured output with pure content + metadata:
    ```json
    {
      "path": "file.txt",
      "content": "...",
      "lines_shown": 2000,
      "total_lines": 5000,
      "truncated": true
    }
    ```
  - [ ] `truncated: true` only when `lines_shown < total_lines` (not for char truncation)
  - [ ] Update existing tests, add line truncation tests
- ✅ **Demo**:
  - Create 3000-line file → Read returns 2000 lines, `total_lines: 3000`, `truncated: true`
  - Create file with 1000-char line → Line silently truncated at 500 chars, `truncated: false`
  - `cargo test -p zdx-core read` passes
- **Risks / failure modes**:
  - Binary files misdetected as text → mitigated by existing image detection
  - Memory usage for line collection → acceptable for 2000 lines
  - Huge single-line files → hard byte limit per line prevents OOM

## Slice 2: Read tool - offset/limit parameters

- **Goal**: Allow AI to request specific portions of truncated files
- **Scope checklist**:
  - [ ] Add `offset` parameter (1-indexed line number, default 1)
  - [ ] Add `limit` parameter (max lines to return, default 2000)
  - [ ] Update tool schema with new parameters
  - [ ] Update output to include `offset` in response
  - [ ] Add tests for offset/limit combinations
- ✅ **Demo**:
  - Read with `offset: 1000, limit: 500` returns lines 1000-1499
  - AI can "page through" a large file
  - `cargo test -p zdx-core read` passes
- **Risks / failure modes**:
  - Off-by-one errors → careful 1-indexed handling + tests
  - Offset beyond file → return empty with `total_lines` info

## Slice 3: Bash tool - basic output truncation

- **Goal**: Truncate large command outputs to prevent context overflow
- **Scope checklist**:
  - [ ] Add constants: `MAX_OUTPUT_BYTES = 50 * 1024` (50KB per stream)
  - [ ] Truncate stdout/stderr independently at byte limit (at UTF-8 char boundary)
  - [ ] Add metadata fields (not inline markers):
    ```json
    {
      "stdout": "...",
      "stderr": "...",
      "exit_code": 0,
      "timed_out": false,
      "stdout_truncated": true,
      "stderr_truncated": false,
      "stdout_total_bytes": 102400,
      "stderr_total_bytes": 256
    }
    ```
  - [ ] Update tests
- ✅ **Demo**:
  - Run `cat /dev/urandom | head -c 100000 | base64` → stdout truncated, `stdout_truncated: true`
  - `cargo test -p zdx-core bash` passes
- **Risks / failure modes**:
  - Truncation at invalid UTF-8 boundary → truncate at valid char boundary

---

# Contracts (guardrails)

1. **ToolOutput format unchanged**: `{ok: true, data: {...}}` structure preserved
2. **Truncation always signaled**: When file/output is truncated, output includes metadata flag
3. **Backward compatible**: Existing Read/Bash calls without new params work as before
4. **No silent data loss for files**: AI always knows if it's seeing partial file (line truncation is silent)
5. **Pure content**: `content`/`stdout`/`stderr` fields contain only actual file/command output, no synthetic markers

# Key decisions (decided)

1. **Line limits vs byte limits for Read**: Use line limits (2000 lines) - matches codex-rs, more useful for code
2. **No inline markers**: Truncation info in metadata fields only - keeps content pure for parsing/patching
3. **Default offset**: 1-indexed (like codex-rs) for human readability
4. **Stdout/stderr truncation**: Independent limits (50KB each) - simpler reasoning
5. **Always scan for total_lines**: Simpler code (one loop, one path), accept O(n) cost for large files
6. **Silent line char-truncation**: AI doesn't need to know; uses Bash for huge single-line edge cases (codex approach)
7. **YAGNI fields removed**: No `bytes`, no `line_truncation_count`, no `max_line_length` parameter
8. **`truncated` semantics**: Only `true` when `lines_shown < total_lines`, not for char-level truncation
9. **Memory-safe line reading**: Hard byte limit of `MAX_LINE_LENGTH * 4 = 2000 bytes` per line to prevent OOM
10. **Offset beyond file**: Return `content: ""`, `lines_shown: 0`, `total_lines: N`, `truncated: false`
11. **Empty files**: Return `content: ""`, `lines_shown: 0`, `total_lines: 0`, `truncated: false`
12. **Line endings**: Preserve as-is (no CRLF→LF normalization)
13. **Invalid UTF-8**: Use `String::from_utf8_lossy` (replace bad bytes with �)

# Testing

- **Manual smoke demos per slice**: Listed in ✅ Demo sections
- **Minimal regression tests**:
  - `test_read_large_file_truncated` → update for line-based
  - `test_read_line_truncation` → new (verify silent char truncation)
  - `test_read_offset_limit` → new
  - `test_read_huge_single_line` → new (verify no OOM)
  - `test_bash_stdout_truncated` → new

---

# Polish phases (after MVP)

## Phase 1: Bash temp file storage (deferred feature)
- Write full output to temp file when truncated
- Include temp file path in response for AI to Read with offset
- Temp files in OS temp dir with session-scoped cleanup
- ✅ Check-in: AI can recover full bash output via Read tool

---

# Later / Deferred

| Item | Trigger to revisit |
|------|-------------------|
| Token-based truncation | If byte/line limits cause context issues with specific models |
| Head+tail preservation | If AI frequently needs end-of-file content |
| Configurable limits per model | If defaults don't work for specific use cases |
| Streaming truncation | If real-time output display is needed |
| `max_line_length` parameter | If AI needs to disable char truncation for specific reads |
| SPEC.md update | After MVP ships and behavior is validated |