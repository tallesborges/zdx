# plan_edit_tool

> **Goal:** Add a deterministic filesystem `edit` tool (exact replace) with clear error behavior.
>
> **Contract impact:** Adds a new tool (`edit`) to the tools contract in SPEC §6.

---

## Step 1: Specify the edit tool contract in SPEC

**Commit:** `docs: add edit tool contract`

**Goal:** Document `edit` in the tools list and define its input/output schema and behavior.

**Deliverable:**
- SPEC tool list includes `edit` (planned)
- New `edit` tool definition with:
  - input schema (`path`, `old`, `new`, `expected_replacements` default=1)
  - output schema (`path`, `replacements`)
  - behavior:
    - resolves `path` per SPEC path rules (relative to `--root`)
    - reads file as UTF-8 text (no newline normalization)
    - counts non-overlapping occurrences of `old`
    - enforces `expected_replacements`
  - stable error codes for common failures

**Files changed:**
- `docs/SPEC.md`

**Verification:** `rg -n "#### \\`edit\\`" docs/SPEC.md`

**Edge cases covered (documented):**
- `old == ""` is rejected
- `expected_replacements < 1` is rejected
- exact match semantics (CRLF/LF not normalized)
- non-UTF-8 file read fails deterministically

---

## Step 2: Implement edit tool + registry wiring

**Commit:** `feat(tools): add edit tool`

**Goal:** Implement the edit tool and register it with the tool system.

**Deliverable:**
- `src/tools/edit.rs` with:
  - tool definition (name, description, JSON schema)
  - execution:
    - parse/validate input
    - resolve path (absolute vs relative-to-root)
    - read file to string
    - count matches
    - fail without writing if count is 0 or mismatched
    - write updated content on success
  - structured `ToolOutput` envelope for success/failure
- `src/tools/mod.rs` updated to:
  - `pub mod edit;`
  - include `edit::definition()` in `all_tools()`
  - route `edit` through the same blocking + timeout path as `read`/`write`

**Files changed:**
- `src/tools/edit.rs`
- `src/tools/mod.rs`

**CLI demo:**
- `cargo run -- --no-save exec --root . -p "In README.md, replace 'foo' with 'bar' exactly once."`

**Edge cases covered:**
- file unchanged on validation failures (`old_not_found` / `replacement_count_mismatch`)
- error codes stay stable across OSes (tests assert `error.code`, not OS strings)

---

## Step 3: Unit tests for edit (contract-level)

**Commit:** `test(tools): add edit unit tests`

**Goal:** Lock in the `edit` tool’s observable contract (replacement counting + error codes).

**Deliverable:**
- Unit tests in `src/tools/edit.rs` using `tempfile::TempDir` + `ToolContext`

**Tests added/updated:**
- success: one match with default expected=1 → `ok:true`, `replacements:1`, file updated
- failure: zero matches → `ok:false`, `code:"old_not_found"`, file unchanged
- failure: two matches with expected=1 → `ok:false`, `code:"replacement_count_mismatch"`, file unchanged
- failure: invalid input (`old==""` or `expected_replacements==0`) → `ok:false`, `code:"invalid_input"`

**Verification:** `cargo test -p zdx-cli tools::edit`

---

## Step 4: Integration test for tool loop edit

**Commit:** `test: tool loop edits file`

**Goal:** Prove the tool loop executes `edit`, sends `tool_result`, and the file changes on disk.

**Deliverable:**
- Add a test to `tests/tool_use_loop.rs` that:
  - creates a temp `--root` dir with a file (known content)
  - mock server returns a `tool_use` for `edit`
  - CLI runs and performs the edit
  - asserts the file content changed as expected
  - asserts the second provider request contains a `tool_result` referencing the correct `tool_use_id`

**Files changed:**
- `tests/tool_use_loop.rs`

**Verification:** `cargo test --test tool_use_loop`
