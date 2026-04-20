# plan_bash_tool_debug_lines

> **Goal:** Show bash tool request + completion details on stderr using existing tool events.
>
> **Contract impact:** Adds explicit stderr lines for bash tool request/finish details.

---

## Step 1: Document stderr debug lines for bash tool events

**Commit:** `docs: specify bash tool debug lines on stderr`

**Goal:** Update SPEC to document that bash tool request/finish details appear on stderr.

**Deliverable:**
- SPEC output channel rules mention:
  - tool status lines on stderr include tool request/finish details
  - bash tool lines include command on request and exit/timed_out on finish

**Files changed:**
- `docs/SPEC.md`

**Verification:** `rg -n "bash" docs/SPEC.md`

---

## Step 2: Emit bash tool request details on ToolRequested

**Commit:** `feat(renderer): show bash command when tool requested`

**Goal:** Print a compact stderr line showing the bash command when ToolRequested fires.

**Deliverable:**
- Renderer records tool_use `id -> name`
- On `ToolRequested` for `bash`, emit:
  - `Tool requested: bash command="..."`

**Files changed:**
- `src/renderer.rs`

**CLI demo:**
- `cargo run -- --no-save exec -p "run a bash command"`

**Edge cases:**
- Missing/invalid `command` field → omit command detail
- Non-bash tools → no extra line

---

## Step 3: Emit bash tool finish details on ToolFinished

**Commit:** `feat(renderer): show bash exit info on tool finish`

**Goal:** Print a compact stderr line showing exit code and timeout when ToolFinished fires.

**Deliverable:**
- On `ToolFinished` for `bash`, emit:
  - `Tool finished: bash exit=0` (or `timed_out=true`)

**Files changed:**
- `src/renderer.rs`

**Tests added/updated:**
- `tests/tool_use_loop.rs`: assert stderr contains `Tool requested: bash command=...`
  and `Tool finished: bash exit=...` for a bash tool call

**CLI demo:**
- `cargo run -- --no-save exec -p "run a bash command"`

**Edge cases:**
- Tool failure → include `error=...` instead of exit info
- Tool finish without prior request → no extra line
