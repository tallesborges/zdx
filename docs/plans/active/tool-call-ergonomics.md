# tool-call-ergonomics

> **Goal:** Reduce repeated tool-call schema-mismatch failures (CLI-style `-i`/`-C` flags, and `path` vs `file_path`) by improving leaf-tool error messages and accepting a small set of CLI-style aliases. Standardize `file_path` across `grep` and `glob`.
>
> **Contract impact:** No new tool surface. Tools return more informative `invalid_input` errors with valid-field lists and suggestions. `grep` accepts a closed set of CLI-shape aliases (`-i`, `-C`). `glob`'s `path` parameter is renamed to `file_path` (breaking the old shape, but with corrective errors guiding callers). SPEC gains a small note codifying the boundary contract.

## Background

Recent thread `5813e02d-7bb4-409c-9910-5aec247a808c` showed 8 sequential `grep` failures across two patterns:

- 4× `Parse error: unknown field -i` (CLI flag instead of `case_insensitive`)
- 4× `Parse error: unknown field path` (used `path` instead of `file_path`)

A broader audit of saved threads confirmed `path` is the most common schema mismatch (7+ hits across multiple threads) but **also showed the agent self-corrects in ~1 retry** once it sees an `invalid_input` error. The remaining cost is the unhelpfulness of today's `Parse error: unknown field path` wording, not the lack of an alias.

The structural fixes:

1. Make the error message corrective (lists valid fields, suggests the closest one). This alone handles `path` and most other one-off mistakes.
2. Accept a small set of CLI-shape aliases (`-i`, `-C`) where the model's mental model is "I'm calling rg" rather than "I'm sending JSON" — the corrective error is less natural for these because nothing edit-distance-close.
3. Standardize naming so the cross-tool inconsistency (`grep.file_path` vs `glob.path`) goes away.

An existing regression test (`crates/zdx-tools/src/grep.rs:1898`) deliberately rejects `path` aliasing on grep. That decision is preserved: corrective errors are a better fix than aliasing legacy field names.

## Non-goals

- No engine-level "repeated failure circuit breaker" in this plan. Defer until after Steps 1–3 ship; revisit if future failures show the same `(tool, unknown_field)` pair recurring.
- No new prompt-side translation guides for shell→JSON; the tool errors carry the guidance instead.
- No alias-hit telemetry yet. `zdx-tools` has no `tracing` usage today; adding a logging facility is a separate change.
- **No `path` alias on `grep` or `glob`**. The existing `test_legacy_path_key_is_rejected` regression encodes a deliberate prior decision; corrective errors handle the failure mode in one retry.
- Aliases that *do* land are a **closed, documented** list, not a fuzzy-match layer.
- No fix for `type: "kt"` extension fallback — already works correctly today (verified by `test_type_filter_unknown_type_falls_back_to_extension_match`).

---

## Step 1: Corrective `invalid_input` errors with valid-field list and suggestion

**Commit:** `feat(tools): corrective invalid_input errors list valid fields`

**Goal:** When a leaf tool fails to deserialize input, return an `invalid_input` `ToolOutput` that names the unknown field(s), lists the valid fields for that tool, suggests the closest valid field, and includes a canonical example.

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
  - For each unknown key, compute the closest valid field via inline Levenshtein (≤ 2) or prefix/substring match. Skip suggestion when nothing is close.
  - **Fallback:** when `raw_input` is not an object, or all keys are valid (i.e. type mismatch), include `parse_err` text in `details`.
  - For tools with alias normalization, pass the **post-normalization** candidate value to `schema_error`; `raw_input` means "the value attempted for serde," not necessarily the original tool-call payload. Otherwise `{"pattern": 123, "path": "src"}` could normalize successfully (`path` → `file_path`), fail on `pattern`'s type, and the helper would still incorrectly flag `path` as unknown if given the pre-normalization input.
  - Output shape:
    - message: `Invalid input for grep: unknown field 'path'. Did you mean 'file_path'?` (highlights the first unknown field)
    - details: `Valid fields: pattern, file_path, glob, case_insensitive, context_lines, max_count, offset, extract_unique, type. Other unknown fields: <list>. Example: {"pattern":"foo","file_path":"src/"}`
    - When no example given: omit the `Example:` line.
- Wire `grep::execute` to use the helper at the parse-failure site (`grep.rs:295-303`). Pass an example like `{"pattern":"foo","file_path":"src/"}`.

**Files changed:**

- `crates/zdx-tools/src/lib.rs` (helper + inline Levenshtein, ~40 LoC, no new deps)
- `crates/zdx-tools/src/grep.rs` (use helper)

**Tests (in `grep.rs`):**

- `test_legacy_path_key_suggests_file_path`: input `{pattern, path}` → returns `invalid_input`, message contains `unknown field 'path'`, `Did you mean 'file_path'`, and `Valid fields:`. **This strengthens (does not replace) the existing `test_legacy_path_key_is_rejected`** — the old test asserts rejection; the new test asserts the rejection message is corrective. Both stay.
- `test_unknown_field_unknown_cli_flag_no_suggestion`: input includes `-Z` (no close match, not an alias) → message lists valid fields without a "did you mean", does not panic.
- `test_multiple_unknown_fields`: input with two unknown fields → primary message highlights one; details lists the other.
- `test_non_object_input`: input is a JSON array/string → falls back to parse error in details, still includes `Valid fields:`.
- `test_type_mismatch_no_unknown_field`: e.g. `{"pattern": 123}` → falls back to parse error, no spurious "unknown field" wording.
- `test_serialized_field_name_type`: input `{"pattern": "x", "file_type": "rust"}` (Rust field name leaked) → message reports `unknown field 'file_type'`, suggests `type` (the JSON name).
- Keep existing `test_deny_unknown_fields_rejects_extra_keys` passing.

**Verification:**

- Use the Grep tool to confirm no other tool currently relies on the old `Parse error: {e}` text shape.
- `cargo test -p zdx-tools grep`
- `cargo clippy -p zdx-tools`

---

## Step 2: Accept CLI-style aliases for `grep`

**Commit:** `feat(grep): accept CLI-style flags as schema input`

**Goal:** Stop CLI-style mistakes (`-i`, `-C`) from failing at all. These are mental-model failures (the agent thinks it's calling `rg`), not legacy field-name confusion. Step 1's corrective error is awkward for them because there's no edit-distance-close suggestion ("Did you mean `case_insensitive`?" is a stretch from `-i`).

**Rationale:** CLI flags are a distinct failure shape from legacy field names. For legacy field names (`path`), Step 1's "Did you mean 'file_path'?" suggestion is natural and one retry fixes it. For CLI flags, aliasing prevents a failure that's harder to self-correct from.

**Aliases (grep only):**

| Alias  | Canonical field      | Notes                                 |
| ------ | -------------------- | ------------------------------------- |
| `-i`   | `case_insensitive`   | CLI-style; coerce truthy → `true`     |
| `-C`   | `context_lines`      | CLI-style; integer or string-int      |

**Deliberately excluded:**

- `path` → `file_path`. Existing regression test deliberately rejects this; Step 1's corrective error handles it in one retry.
- `-A` / `-B`. Asymmetric grep concepts; the tool only supports symmetric `context_lines`. "Use the larger value" would be lossy and surprising.
- `pattern_re`, `regex`, `query`, `--type`. Speculative. Step 1's corrective errors handle these by suggesting the canonical name.

**Deliverable:**

- New helper in `crates/zdx-tools/src/grep.rs` (kept tool-local for now):

  ```rust
  fn normalize_aliases(input: &mut serde_json::Value) -> Result<(), ToolOutput>
  ```

  - Operates only on top-level object keys.
  - **Conflict policy:**
    - If both alias and canonical are present with **different** values → return `invalid_input`: `Conflicting fields '-i' and 'case_insensitive'; use one.`
    - If both present with **identical** values → silently drop the alias.
    - If alias value is empty/whitespace → drop, do not overwrite canonical.
  - Canonical never silently wins on conflict — that would hide ambiguous model intent and make debugging harder.
- **Wiring in `grep::execute`:** clone `input` into a mutable `normalized_input`, call `normalize_aliases(&mut normalized_input)?`, deserialize from `normalized_input.clone()`, and on parse failure call `schema_error(..., &normalized_input)` so suggestions reflect the value actually attempted.

**Files changed:**

- `crates/zdx-tools/src/grep.rs`

**Tests:**

- `test_alias_dash_i_normalized_to_case_insensitive` (boolean and string `"true"`)
- `test_alias_dash_c_normalized_to_context_lines`
- `test_alias_conflict_returns_invalid_input` (both `-i` and `case_insensitive` present and different)
- `test_alias_canonical_wins_when_alias_empty`
- `test_alias_identical_values_dropped_silently`
- **Keep** `test_legacy_path_key_is_rejected` (proves `path` is still rejected, complementing Step 1's `test_legacy_path_key_suggests_file_path`).

**Verification:** `cargo test -p zdx-tools grep`

---

## Step 3: Standardize `path` → `file_path` in `glob` (no alias)

**Commit:** `refactor(glob): rename 'path' parameter to 'file_path'`

**Goal:** Remove the cross-tool inconsistency that made the original grep mistake predictable. After this step, every leaf tool that takes a directory/file argument uses `file_path`. Old callers sending `path` to `glob` get an `invalid_input` from Step 1's `schema_error`, with a clear `Did you mean 'file_path'?` suggestion — single retry to recover.

**Rationale:** The audit shows agents self-correct in ~1 retry given a clear error. Adding a permanent alias would (a) violate AGENTS' "no compatibility shims" rule, (b) re-introduce the cross-tool inconsistency this step is fixing, just in reverse, and (c) hide the rename from any future code reading the schema.

**Deliverable:**

- Rename `GlobInput.path` → `GlobInput.file_path` in `crates/zdx-tools/src/glob.rs`.
- **Add `#[serde(deny_unknown_fields)]` to `GlobInput`** (it currently lacks it). Without this, post-rename calls using `path` would silently succeed with no narrowing.
- Update the JSON schema in `glob::definition` to advertise `file_path` (description text reused).
- Use the new `schema_error` helper in `glob::execute` for parse failures, with the same wiring as Step 2: deserialize, on parse failure pass the input value to `schema_error`. (No alias normalization layer needed — there are no aliases on glob.)

**Files changed:**

- `crates/zdx-tools/src/glob.rs`

**Tests:**

- Update existing `test_explicit_path` and `test_nonexistent_path` to use `file_path` (canonical).
- `test_glob_legacy_path_key_suggests_file_path`: `{"pattern": "*.txt", "path": "sub"}` → returns `invalid_input` whose message contains `unknown field 'path'`, `Did you mean 'file_path'`, and `Valid fields:`. Mirrors grep's regression test.
- `test_glob_unknown_field_rejected`: `{"pattern": "*.txt", "directory": "sub"}` → returns `invalid_input` with valid-fields list (proves `deny_unknown_fields` + `schema_error` are wired).

**Verification:**

- `cargo test -p zdx-tools glob`
- Use the Grep tool to confirm no other code asserts on `glob`'s literal `"path"` JSON key.
- `just ci`

---

## Step 4: Update SPEC + tighten prompt; drop bad advice

**Commit:** `docs: codify tool-input boundary aliases and corrective errors`

**Goal:** Document the new boundary contract in `docs/SPEC.md`, tighten the prompt-side reflection rule so it explicitly addresses parse errors, and remove any advice that suggests falling back to `bash`+`rg` for native search tools.

**Deliverable:**

- **`docs/SPEC.md`** — add a small subsection (≤ 10 lines) under the existing tool-output section:
  - Built-in tools that fail to deserialize their input return `code: "invalid_input"` and, when the input is an object, include the valid field list and (when close) a suggested field in `details`.
  - Built-in tools may normalize a closed, documented set of CLI-shape input aliases (e.g. `-i`, `-C`) at the tool boundary. Aliases are not advertised in the JSON schema. Conflicting alias + canonical values fail with `invalid_input`.
  - Legacy field-name renames (e.g. `glob.path` → `glob.file_path`) do **not** carry aliases; old callers receive a corrective `invalid_input` and recover via retry.
- **`crates/zdx-assets/prompts/system_prompt_template.md`** — replace the existing schema-reflection wording (around `:99-104`) with:

  > On `invalid_input` / parse errors, MUST compare the attempted keys against the tool's valid fields (listed in the error) before retrying. MUST NOT resend the same tool call with the same invalid key set; either change the invalid fields or stop and report the blocker.

  Do NOT add a shell→JSON translation table to the prompt; the tool error now carries the same information.
- **Audit and remove bash-fallback advice.** Use the Grep tool to find candidates:
  - Search `crates/zdx-assets` for: `bash.*rg`, `rg.*bash`, `fall.*back.*bash`, `rg --files`, `rg -l`.
  - Remove or rewrite. Keep the existing positive guidance ("NEVER invoke grep or rg as a Bash command") which lives in the `Grep` tool description (`crates/zdx-tools/src/grep.rs:45`).

**Files changed:**

- `docs/SPEC.md`
- `crates/zdx-assets/prompts/system_prompt_template.md`

**Verification:**

- Use the Grep tool to confirm no remaining bash-fallback wording in `crates/zdx-assets` or `docs/`.
- `cargo test -p zdx-assets` (if any tests pin prompt content)
- `just ci`

---

## Out of scope / follow-ups

- **Repeated-invalid-call circuit breaker** in the engine tool dispatcher. Implementable but skipped here — Steps 1–3 should remove the underlying failure mode. Revisit if future failures show the same `(tool, unknown_field)` pair recurring within a thread.
- **Other tools.** This plan touches only `grep` and `glob`. `read`, `write`, `edit` already use `file_path` and are not implicated. If/when another tool surfaces a recurring alias mistake, extend Step 2's pattern there.
- **Edit-distance suggestions for non-grep tools.** The Step 1 helper is reusable; wire it into other tools opportunistically as part of unrelated changes.
- **Alias-hit observability.** Add structured logging when a `tracing` setup is introduced in `zdx-tools` (tool name, alias, canonical, conflict/no-conflict, no values).

---

## Risk and verification

- **Risk:** Renaming `glob.path` → `glob.file_path` breaks any caller that hard-codes the JSON key `"path"`. Mitigation: corrective `schema_error` immediately suggests `file_path`; agents recover in one retry. Search confirmed there are no in-tree consumers asserting on the literal key.
- **Risk:** Adding `deny_unknown_fields` to `glob` could reject a previously-tolerated extra key. Mitigation: there are no known extra keys in use; failures will surface immediately with a corrective `schema_error` message.
- **Risk:** Step 2 conflict policy (`-i` + `case_insensitive` with different values fails) introduces a new error path. Covered by `test_alias_conflict_returns_invalid_input`.
- **Risk:** `schema_error`'s edit-distance heuristic suggests the wrong field. Mitigation: tests cover the documented case (`path` → `file_path`); unknown short flags like `-Z` produce no suggestion rather than a misleading one.

Final verification after Step 4: `just ci`, plus a manual smoke by sending `{"pattern":"foo","-i":true}` to grep and `{"pattern":"*.rs","path":"src"}` to glob, confirming the first succeeds (alias) and the second fails with a corrective `Did you mean 'file_path'?` message.
