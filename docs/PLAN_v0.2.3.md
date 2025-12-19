# PLAN v0.2.3 — Write Tool

> **Goal:** Add a deterministic filesystem `write` tool with clear error behavior.
>
> **Contract impact:** Adds a new tool (`write`) to the tools contract in SPEC §6.

---

## Step 1: Specify the write tool contract in SPEC

**Commit:** `docs: add write tool contract`

**Goal:** Document `write` in the tools list and define its input/output schema and behavior.

**Deliverable:**
- SPEC tool list includes `write`
- New `write` tool definition with:
  - input schema (`path`, `content`)
  - output schema (`path`, `bytes`, `created`)
  - behavior (creates or overwrites files, no implicit mkdirs, path resolution, error codes)

**Files changed:**
- `docs/SPEC.md`

**Verification:** `rg -n "write" docs/SPEC.md`

**Edge cases covered:**
- Missing parent directory → `path_error`
- Overwrite vs create semantics
- Invalid input error shape

---

## Step 2: Implement write tool + registry wiring

**Commit:** `feat(tools): add write tool`

**Goal:** Implement the write tool and register it with the tool system.

**Deliverable:**
- `src/tools/write.rs` with:
  - tool definition (name, description, JSON schema)
  - execution that writes content to the resolved path
  - deterministic tool output envelope
- `src/tools/mod.rs` updated to:
  - include `write` in `all_tools()`
  - execute `write` with the same timeout behavior as `read`

**Files changed:**
- `src/tools/write.rs`
- `src/tools/mod.rs`

**Tests added/updated:**
- `src/tools/write.rs` unit tests:
  - writes a new file and returns `bytes`
  - overwrites an existing file (`created=false`)
  - invalid input returns `invalid_input`

**CLI demo:**
- `cargo run -- --no-save exec -p "Write hello.txt with 'hello world'"`

**Edge cases covered:**
- Absolute vs relative path resolution
- Parent directory missing → `path_error`
- Permission/IO failures surfaced as `write_error`

---

## Step 3: Integration test for tool loop write

**Commit:** `test: tool loop writes file`

**Goal:** Prove the tool loop executes `write` and returns a tool_result in the second request.

**Deliverable:**
- Integration test that:
  - mocks a `tool_use` for `write`
  - runs CLI with `--root` in a temp dir
  - asserts the file exists with expected content
  - asserts the second provider request contains `tool_result`

**Files changed:**
- `tests/tool_use_loop.rs`

**Tests added/updated:**
- `cargo test --test tool_use_loop`

**Edge cases covered:**
- Tool result contains `ok:true` and expected `bytes`
- File content matches the provided `content` string
