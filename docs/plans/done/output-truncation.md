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

## Slice 1: Read tool - line-based truncation ✅

- **Goal**: Replace byte truncation with line-aware truncation (like codex-rs)
- **Scope checklist**:
  - [x] Add constants: `MAX_LINES = 2000`, `MAX_LINE_LENGTH = 500`
  - [x] Implement `read_text()` that reads line-by-line with limits
  - [x] Use hard byte limit per line to prevent memory issues with huge single-line files
  - [x] Truncate individual lines exceeding `MAX_LINE_LENGTH` at char boundary (silent, no marker)
  - [x] Always scan entire file for `total_lines` (simpler code, one path)
  - [x] Return structured output with pure content + metadata:
    ```json
    {
      "path": "file.txt",
      "content": "...",
      "lines_shown": 2000,
      "total_lines": 5000,
      "truncated": true
    }
    ```
  - [x] `truncated: true` only when `lines_shown < total_lines` (not for char truncation)
  - [x] Update existing tests, add line truncation tests
- ✅ **Demo**:
  - Create 3000-line file → Read returns 2000 lines, `total_lines: 3000`, `truncated: true`
  - Create file with 1000-char line → Line silently truncated at 500 chars, `truncated: false`
  - `cargo test -p zdx-core read` passes
- **Risks / failure modes**:
  - Binary files misdetected as text → mitigated by existing image detection
  - Memory usage for line collection → acceptable for 2000 lines
  - Huge single-line files → hard byte limit per line prevents OOM

## Slice 2: Read tool - offset/limit parameters ✅

- **Goal**: Allow AI to request specific portions of truncated files
- **Scope checklist**:
  - [x] Add `offset` parameter (1-indexed line number, default 1)
  - [x] Add `limit` parameter (max lines to return, default 2000)
  - [x] Update tool schema with new parameters
  - [x] Update output to include `offset` in response
  - [x] Add tests for offset/limit combinations
- ✅ **Demo**:
  - Read with `offset: 1000, limit: 500` returns lines 1000-1499
  - AI can "page through" a large file
  - `cargo test -p zdx-core read` passes (53 tests)
- **Risks / failure modes**:
  - Off-by-one errors → mitigated by careful 1-indexed handling + tests
  - Offset beyond file → returns `content: ""`, `lines_shown: 0`, `total_lines: N`, `truncated: false`

## Slice 3: Bash tool - basic output truncation ✅

- **Goal**: Truncate large command outputs to prevent context overflow
- **Scope checklist**:
  - [x] Add constants: `MAX_OUTPUT_BYTES = 40 * 1024` (40KB per stream)
  - [x] Truncate stdout/stderr independently at byte limit (at UTF-8 char boundary)
  - [x] Add metadata fields (not inline markers):
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
  - [x] Update tests
- ✅ **Demo**:
  - Run `cat /dev/urandom | head -c 100000 | base64` → stdout truncated, `stdout_truncated: true`
  - `cargo test -p zdx-core bash` passes (13 tests)
- **Risks / failure modes**:
  - Truncation at invalid UTF-8 boundary → mitigated by `truncate_at_utf8_boundary()` helper

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
4. **Stdout/stderr truncation**: Independent limits (40KB each) - simpler reasoning
5. **Always scan for total_lines**: Simpler code (one loop, one path), accept O(n) cost for large files
6. **Silent line char-truncation**: AI doesn't need to know; uses Bash for huge single-line edge cases (codex approach)
7. **YAGNI fields removed**: No `bytes`, no `line_truncation_count`, no `max_line_length` parameter
8. **`truncated` semantics**: Only `true` when `lines_shown < total_lines`, not for char-level truncation
9. **Memory-safe line reading**: Hard byte limit of `MAX_LINE_LENGTH * 4 = 2000 bytes` per line to prevent OOM
10. **Offset beyond file**: Return `content: ""`, `lines_shown: 0`, `total_lines: N`, `truncated: false`
11. **Empty files**: Return `content: ""`, `lines_shown: 0`, `total_lines: 0`, `truncated: false`
12. **Line endings**: Preserve as-is (no CRLF→LF normalization)
13. **Invalid UTF-8**: Use `String::from_utf8_lossy` (replace bad bytes with �)
14. **Secondary byte limit for Read**: 40KB per page, matching Bash tool limit, to prevent context bloat from long lines

# Testing

- **Manual smoke demos per slice**: Listed in ✅ Demo sections
- **Minimal regression tests**:
  - `test_read_large_file_truncated` → updated for line-based ✅
  - `test_read_line_truncation` → new (verify silent char truncation) ✅
  - `test_read_huge_single_line` → new (verify no OOM) ✅
  - `test_read_with_offset` → new ✅
  - `test_read_with_limit` → new ✅
  - `test_read_with_offset_and_limit` → new ✅
  - `test_read_offset_beyond_file` → new ✅
  - `test_read_offset_zero_treated_as_one` → new ✅
  - `test_read_limit_capped_at_max` → new ✅
  - `test_read_paging_through_file` → new ✅
  - `test_bash_stdout_truncated` → new ✅
  - `test_bash_stderr_truncated` → new ✅
  - `test_bash_no_truncation_under_limit` → new ✅
  - `test_truncate_at_utf8_boundary_*` → new (UTF-8 boundary unit tests) ✅
  - `test_format_byte_truncation_*` → new (byte formatting helper tests) ✅
  - `test_tool_bash_truncation_warning_displayed` → new (TUI warning display) ✅
  - `test_tool_bash_stderr_truncation_warning` → new (TUI stderr warning) ✅
  - `test_tool_read_truncation_warning_displayed` → new (TUI file truncation warning) ✅
  - `test_tool_no_truncation_no_warning` → new (no warning when not truncated) ✅
  - `test_truncation_warning_style` → new (verify correct style applied) ✅
  - `test_bash_stdout_truncated_writes_temp_file` → new (Phase 1: verify temp file creation for stdout) ✅
  - `test_bash_stderr_truncated_writes_temp_file` → new (Phase 1: verify temp file creation for stderr) ✅
  - `test_bash_no_truncation_no_temp_file` → new (Phase 1: no temp file when not truncated) ✅
  - `test_write_temp_file` → new (Phase 1: temp file helper unit test) ✅
  - `test_read_byte_limit_with_long_lines` → new (Phase 2: verify byte limit with 200-char lines) ✅
  - `test_read_line_limit_before_byte_limit` → new (Phase 2: verify line limit wins for short lines) ✅
  - `test_read_no_truncation_byte_limited_false` → new (Phase 2: verify byte_limited=false when not truncated) ✅

---

# Polish phases (after MVP)

## Phase 0: TUI truncation warnings ✅

- **Goal**: Display truncation warnings to users in the TUI transcript when tool output was truncated
- **Scope checklist**:
  - [x] Add `ToolTruncation` style for distinct visual styling (yellow/dim)
  - [x] Display truncation warnings in tool cell output:
    - Bash: Show `[⚠ stdout truncated: X KB total]` when `stdout_truncated: true`
    - Bash: Show `[⚠ stderr truncated: X KB total]` when `stderr_truncated: true`
    - Read: Show `[⚠ file truncated: showing N of M lines]` when `truncated: true`
  - [x] Add `format_byte_truncation()` helper for human-readable byte formatting (bytes/KB/MB)
  - [x] Add unit tests for truncation warning display
- ✅ **Demo**:
  - `cargo test -p zdx-tui truncation` passes (8 tests)
  - Tool cells with truncated output show yellow warning line below output preview
- **Why it matters**: Users can see at a glance when large outputs were truncated, making context window limits visible

## Phase 1: Bash temp file storage ✅

- **Goal**: Write full, un-truncated output to temp files when Bash output is truncated, enabling AI to access the complete data via the Read tool
- **Scope checklist**:
  - [x] Add `write_temp_file()` helper to write bytes to temp file with unique name (zdx-bash-{uuid}-{stream}.txt)
  - [x] Add `stdout_file: Option<String>` and `stderr_file: Option<String>` fields to `BashOutput`
  - [x] When stdout is truncated, write full stdout bytes to temp file and set `stdout_file`
  - [x] When stderr is truncated, write full stderr bytes to temp file and set `stderr_file`
  - [x] Include file paths in JSON response only when present (conditional serialization)
  - [x] Add tests for temp file creation and content verification
- ✅ **Demo**:
  - Run command with >40KB stdout → response includes `stdout_file` path
  - AI can use Read tool on `stdout_file` path with offset/limit to access full data
  - `cargo test -p zdx-core bash` passes (17 tests)
- **Contract**: Temp files are created in OS temp dir (`std::env::temp_dir()`) with zdx-bash-{uuid}-{stream}.txt naming
- **Why it matters**: AI can now recover full command output when truncated, using the Read tool's paging capabilities

## Phase 2: Read tool byte limit ✅

- **Goal**: Enforce a secondary 40KB byte limit per page to prevent context bloat from files with many long lines
- **Scope checklist**:
  - [x] Add `MAX_PAGE_BYTES = 40 * 1024` constant (matches Bash tool)
  - [x] Track accumulated bytes during line collection
  - [x] Stop collecting when either line limit OR byte limit is reached (whichever comes first)
  - [x] Add `byte_limited: bool` field to output (true when byte limit caused truncation)
  - [x] Update `truncated` flag logic: true if either line-limited OR byte-limited
  - [x] Add tests: byte limit with long lines, line limit before byte limit, no truncation
- ✅ **Demo**:
  - File with 300 lines × 200 chars each → byte limit kicks in at ~204 lines, `byte_limited: true`
  - File with 3000 short lines → line limit kicks in at 2000 lines, `byte_limited: false`
  - `cargo test -p zdx-core read` passes (37 tests)
- **Contract**: Read tool now guarantees pages never exceed 40KB, regardless of line count
- **Why it matters**: Prevents edge case where files with extremely long lines could bloat context even within line count limits

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