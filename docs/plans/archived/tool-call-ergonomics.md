# tool-call-ergonomics

> **Status:** Archived/unshipped. The grep rename (`file_path` → `path`) shipped separately and resolved the immediate failure mode. The corrective `invalid_input` error work described below was not implemented; treat it as a design sketch for a future follow-up if `-i`/`-C` or other unknown-field failures recur.

> **Goal:** Replace today's terse `Parse error: {e}` for tool-input failures with a corrective `invalid_input` message that names the unknown field(s), lists the valid fields, suggests the closest match, and shows a canonical example. Codify the contract in `docs/SPEC.md` and tighten the prompt-side reflection rule. No aliases, no new tool surface.

## Background

Saved threads (`5813e02d-7bb4-409c-9910-5aec247a808c`, `48f1ecb5-03f8-48bf-970b-097df6f6ecc6`) show the dominant tool-failure mode is **schema-mismatch parse errors with unhelpful messages**. The two most common shapes were:

- `Parse error: unknown field path` (model used `path` against the legacy `grep.file_path`)
- `Parse error: unknown field -i` (model sent CLI-style flag instead of `case_insensitive`)

Two observations from auditing those threads:

1. The agent self-corrects in ~1 retry once it sees `invalid_input`, but every first-try failure costs a round trip and a confused continuation.
2. The `path`/`file_path` failure was a **cross-tool inconsistency** — `glob` used `path`, `grep` used `file_path`. Resolved separately by renaming `grep.file_path` → `grep.path`; `test_legacy_file_path_key_is_rejected` (`crates/zdx-tools/src/grep.rs:1898`) now locks in the new shape.

After the rename, the remaining cost is the **wording** of the error, not the lack of an alias. Step 1 fixes the wording at the failure point — where the model actually sees it. Step 2 codifies the contract in SPEC + prompt so it stays fixed.

## Deliberate asymmetry: `path` vs `file_path`

After the grep rename, `grep` and `glob` use `path`; `read`/`write`/`edit` still use `file_path`. This is **intentional**:

- `grep`/`glob` accept a directory **or** file → `path` is the natural name.
- `read`/`write`/`edit` always target a single file → `file_path` describes the value precisely.

Future schema changes preserve this distinction: parameters that may be a directory use `path`; parameters that are always a single file use `file_path`. Cross-tool consistency *within* each class is the invariant.

## Non-goals

- **No alias layer for legacy field names.** The `test_legacy_file_path_key_is_rejected` regression encodes a deliberate "no compatibility shim" decision (consistent with `AGENTS.md`). Corrective errors handle the failure in one retry.
- **No alias layer for CLI flags** in this plan. Step 1's `Example:` line already shows the canonical shape; aliasing `-i`/`-C` only earns its complexity if data shows that's still insufficient. See follow-up F1.
- **No prompt-side shell→JSON translation tables.** The tool error carries the same information.
- **No engine-level repeated-failure circuit breaker.** Revisit if the same `(tool, unknown_field)` pair recurs in a thread after Step 1 ships. See follow-up F3.
- **No alias-hit telemetry.** `zdx-tools` has no `tracing` setup. See follow-up F4.
- **No fix for `type: "kt"` extension fallback** — already works (`test_type_filter_unknown_type_falls_back_to_extension_match`).

---

## Step 1: Corrective `invalid_input` errors

**Commit:** `feat(tools): corrective invalid_input errors with valid-field list`

**Goal:** When a leaf tool fails to deserialize input, return an `invalid_input` `ToolOutput` that names the unknown field(s), lists the valid fields, suggests the closest valid field via edit distance, and includes a canonical example. Apply to `grep` and `glob`; the helper is reusable by other tools later.

**Rationale:** The model sees the error at the exact failure point, so a corrective message there outperforms any prompt instruction.

**Deliverable:**

- New helper in `crates/zdx-tools/src/lib.rs`:

  ```rust
  pub fn schema_error(
      tool: &str,
      valid_fields: &[&str],
      example: Option<&str>,
      parse_err: &serde_json::Error,
      raw_input: &serde_json::Value,
  ) -> ToolOutput
  ```

  - **Primary detection:** diff `raw_input`'s top-level object keys against `valid_fields`. Collect *all* unknown keys.
  - For each unknown key, compute the closest valid field via inline Levenshtein (≤ 2) or prefix/substring match. Skip the suggestion when nothing is close.
  - **Fallback:** when `raw_input` is not an object, or all keys are valid (i.e. type mismatch), include `parse_err.to_string()` in `details`.
  - **Composition note:** any future caller that pre-normalizes input (e.g. an alias layer from F1) MUST pass the post-normalization candidate to `schema_error`. Otherwise spurious "unknown field" flags can fire on already-renamed keys.
  - **Output shape:**
    - message: `Invalid input for grep: unknown field 'file_path'. Did you mean 'path'?` (highlights the first unknown field)
    - details: `Valid fields: pattern, path, glob, case_insensitive, context_lines, max_count, offset, extract_unique, type. Other unknown fields: <list>. Example: {"pattern":"foo","path":"src/"}`
    - When `example` is `None`: omit the `Example:` line.

- Wire `grep::execute` and `glob::execute` to use the helper at their parse-failure sites.
  - grep example: `{"pattern":"foo","path":"src/"}`
  - glob example: `{"pattern":"*.rs","path":"src/"}`
- Add `#[serde(deny_unknown_fields)]` to `GlobInput` (`crates/zdx-tools/src/glob.rs:44`); it currently lacks the attribute, so unknown keys silently succeed.

**Files changed:**

- `crates/zdx-tools/src/lib.rs` (helper + inline Levenshtein, ~40 LoC, no new deps)
- `crates/zdx-tools/src/grep.rs` (use helper at parse-failure site)
- `crates/zdx-tools/src/glob.rs` (use helper + `deny_unknown_fields`)

**Tests (in `grep.rs`):**

- `test_legacy_file_path_key_suggests_path`: input `{pattern, file_path}` → `invalid_input`; message contains `unknown field 'file_path'`, `Did you mean 'path'`, and `Valid fields:`. **Strengthens (does not replace)** the existing `test_legacy_file_path_key_is_rejected` — the old test asserts rejection; the new test asserts the rejection message is corrective. Both stay.
- `test_unknown_field_no_close_match`: input includes `-Z` → message lists valid fields without a "did you mean", does not panic.
- `test_multiple_unknown_fields`: input with two unknown fields → primary message highlights one; details lists the other.
- `test_non_object_input`: input is a JSON array/string → falls back to parse error in details, still includes `Valid fields:`.
- `test_type_mismatch_no_unknown_field`: e.g. `{"pattern": 123}` → falls back to parse error, no spurious "unknown field" wording.
- `test_serialized_field_name_type`: input `{"pattern": "x", "file_type": "rust"}` (Rust struct field name leaked) → reports `unknown field 'file_type'`, suggests `type` (the JSON name).
- Keep `test_deny_unknown_fields_rejects_extra_keys` passing.

**Tests (in `glob.rs`):**

- `test_glob_unknown_field_rejected`: `{"pattern": "*.txt", "directory": "sub"}` → `invalid_input` with valid-fields list (proves `deny_unknown_fields` + `schema_error` are wired).
- `test_glob_legacy_file_path_key_suggests_path`: `{"pattern": "*.txt", "file_path": "sub"}` → message contains `unknown field 'file_path'`, `Did you mean 'path'`, mirrors grep's regression.

**Verification:**

- `cargo nextest run -p zdx-tools grep`
- `cargo nextest run -p zdx-tools glob`
- `just ci-fast`

---

## Step 2: Codify the contract in SPEC + prompt

**Commit:** `docs: codify tool-input boundary contract`

**Goal:** Document the corrective error contract in `docs/SPEC.md`, and tighten the prompt's tool-error reflection rule so it explicitly addresses parse errors. Keep both changes small.

**Deliverable:**

- **`docs/SPEC.md`** — add a new `### Input parsing` subsection under `## 9) Tools`, after the existing `### Semantics` block (around `docs/SPEC.md:236`):

  > **Input parsing.** Built-in tools that fail to deserialize their input return `code: "invalid_input"`. When the input is an object, `details` includes the full valid-field list and (when edit-distance close) a `Did you mean '<field>'?` suggestion plus a canonical example. Built-in tools do not normalize legacy field renames into aliases; old callers receive a corrective `invalid_input` and recover via retry.
  >
  > **Path parameter naming.** Tools that accept a directory or file use `path` (`grep`, `glob`). Tools that always target a single file use `file_path` (`read`, `write`, `edit`). New tools follow the same rule.

- **`crates/zdx-assets/prompts/system_prompt_template.md`** — append one bullet to the existing `## Tool Errors` block (`:106-109`):

  > - On `invalid_input` / parse errors, MUST compare the attempted input keys against the tool's valid-field list (echoed in the error). MUST NOT resend the same tool call with the same invalid key set; either change the invalid fields or stop and report the blocker.

  Do not add a shell→JSON translation table; the tool error now carries the same information.

**Files changed:**

- `docs/SPEC.md`
- `crates/zdx-assets/prompts/system_prompt_template.md`

**Verification:**

- `cargo nextest run -p zdx-assets` (if any tests pin prompt content)
- `just ci-fast`

---

## Follow-ups (data-triggered)

Each is a separate commit if/when its trigger fires. Not part of this plan.

### F1: CLI-style aliases for `grep` (`-i`, `-C`)

**Trigger:** Post-Step 1 thread audit shows `-i` or `-C` parse failures recur in ≥ 3 distinct threads over 30 days despite the new `Example:` line. Without that evidence, don't add an alias layer.

**Sketch:** tool-local `normalize_aliases` in `grep.rs`; closed list `-i` → `case_insensitive`, `-C` → `context_lines`; conflict policy = error if alias + canonical present with different values; drop alias if identical or empty. Compose with Step 1 by passing the post-normalization input to `schema_error` on subsequent parse failure.

**Deliberately excluded** (in any future revival of this follow-up): `-A`/`-B` (asymmetric, lossy), `pattern_re`, `regex`, `query`, `--type` — Step 1's corrective error handles these via canonical-name suggestion.

### F2: Reuse `schema_error` in `read`/`write`/`edit`/`apply_patch`

Opportunistic — wire the helper into those tools next time they're touched for unrelated work. No dedicated plan needed.

### F3: Repeated-invalid-call circuit breaker

Engine-level guard against the same `(tool, unknown_field_set)` repeating within one thread. Revisit only if Step 1 + Step 2 don't fully eliminate the failure mode.

### F4: Alias-hit observability

When `tracing` is introduced in `zdx-tools`, log alias normalization events (tool, alias, canonical, conflict/no-conflict). Blocked on F1 actually shipping.

---

## Risk and verification

- **Risk:** `schema_error`'s edit-distance heuristic suggests the wrong field. Mitigation: tests cover the documented case (`file_path` → `path`); unknown short flags like `-Z` produce no suggestion rather than a misleading one.
- **Risk:** Adding `deny_unknown_fields` to `GlobInput` could reject a previously-tolerated extra key. Mitigation: no known consumers send extra keys; failures surface immediately with a corrective `schema_error` message and recover in one retry.

Final verification after Step 2: `just ci`, plus a manual smoke by sending `{"pattern":"foo","file_path":"src"}` to grep and confirming the response message contains `Did you mean 'path'?` and the canonical example.
