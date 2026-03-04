# Artifacts Refactoring — Thread-Scoped Folders

## Status: Planned (2026-03-04)

## Problem

Artifacts (HTML pages, generated images, PDFs) are scattered in ad-hoc locations:
- `~/.agent/diagrams/` (html-page skill)
- `$ZDX_HOME/artifacts/` (imagine command)
- No association between artifacts and the thread/automation that created them

This makes it impossible to:
- Know which artifacts a thread produced
- Clean up artifacts when a thread is deleted
- Reference artifacts from daily logs or memory

## Goals

- Single canonical artifact root: `$ZDX_HOME/artifacts`
- Per-thread artifact directories resolved at runtime
- Artifact dir injected into the agent's `<environment>` block (same as `cwd` and `date`)
- All artifact-producing paths (skills, tools, automations) default to the resolved dir
- Human-browsable folder structure

## Non-goals

- CDN/cloud upload (deferred — Cloudflare R2 idea stays in ZDX Features)
- Artifact indexing or search (future)
- Migration of existing artifacts (manual or one-time script)

## Design

### Path resolution

```
artifact_root = $ZDX_HOME/artifacts
```

Per-context artifact dir:

| Context | Artifact dir |
|---------|-------------|
| TUI / CLI thread | `{artifact_root}/threads/{thread_id}/` |
| Telegram bot thread | `{artifact_root}/threads/{thread_id}/` |
| Automation run | `{artifact_root}/threads/{thread_id}/` (automations already have thread IDs: `automation-{name}-{timestamp}`) |
| No thread (edge case) | `{artifact_root}/scratch/` |

Optional subdirectories (agent creates as needed):
- `images/`, `html/`, `files/`

### Environment injection

`build_prompt_template_vars()` in `context.rs` already produces `cwd` and `date`.
Add `artifact_dir` to `PromptTemplateVars`:

```rust
// In PromptTemplateVars struct:
artifact_dir: String,

// In build_prompt_template_vars():
artifact_dir: resolve_artifact_dir(thread_id),
```

The system prompt template renders it as XML children in `<environment>`:

```
<environment>
  <cwd>{{ cwd }}</cwd>
  <date>{{ date }}</date>
  <artifact_dir>{{ artifact_dir }}</artifact_dir>
</environment>
```

This makes each value easy to reference in prompts and skills (e.g., "save to the path in `<artifact_dir>`").

### Artifact dir resolution function

```rust
// In config::paths:

/// Returns the artifact root directory ($ZDX_HOME/artifacts).
pub fn artifact_root() -> PathBuf {
    zdx_home().join("artifacts")
}

/// Returns the artifact directory for a specific thread.
/// Does NOT create the directory — the agent creates it when needed.
pub fn artifact_dir_for_thread(thread_id: Option<&str>) -> PathBuf {
    let root = artifact_root();
    match thread_id {
        Some(id) => root.join("threads").join(id),
        None => root.join("scratch"),
    }
}
```

### Thread ID propagation

`build_prompt_template_vars()` currently has no thread context. Changes needed:

1. Add `thread_id: Option<&str>` parameter to `build_prompt_template_vars()`
2. Thread this through from `build_effective_system_prompt_with_paths()` and callers
3. Callers (TUI runtime, bot handler, CLI exec) already know the thread ID — just pass it down

Call chain:
```
TUI runtime / Bot handler / CLI exec
  → build_effective_system_prompt_with_paths(..., thread_id)
    → build_prompt_template_vars(..., thread_id)
      → artifact_dir = artifact_dir_for_thread(thread_id)
```

## Skill updates

After `artifact_dir` is in `<environment>`, update skills to reference it:

- **html-page skill**: Change default output from `~/.agent/diagrams/` → `{artifact_dir}/`
- **imagine skill**: Already uses `$ZDX_HOME/artifacts` — change to `{artifact_dir}/`
- **screenshot skill**: Output to `{artifact_dir}/` instead of temp paths
- **Automations**: Use `{artifact_dir}/` for generated reports (interactions HTML, morning report HTML)

Skills read `artifact_dir` from the `<environment>` block — no code change in skills, just prompt update.

## MVP slices

### Slice 1: Artifact path resolution + env injection
- Add `artifact_root()` and `artifact_dir_for_thread()` to `config::paths`
- Add `artifact_dir` to `PromptTemplateVars`
- Thread `thread_id` through the prompt-building call chain
- Render `Artifact directory:` in `<environment>` block
- **Demo:** Start a thread, see `Artifact directory: ~/.zdx/artifacts/threads/<id>/` in prompt

### Slice 2: Update skills to use artifact_dir
- Update html-page skill SKILL.md to use `artifact_dir` from environment
- Update imagine command default dir
- Update screenshot skill
- **Demo:** Generate an HTML page → appears in thread's artifact folder

### Slice 3: Update automations
- Update `zdx-daily-interactions-summary` to save HTML to artifact_dir
- Update `morning-report` to save HTML to artifact_dir
- **Demo:** Automation output HTML lands in `artifacts/threads/automation-*/`

## Testing

- Unit test: `artifact_root()` returns `$ZDX_HOME/artifacts`
- Unit test: `artifact_dir_for_thread(Some("abc"))` → `{root}/threads/abc/`
- Unit test: `artifact_dir_for_thread(None)` → `{root}/scratch/`
- Unit test: `PromptTemplateVars` includes `artifact_dir` field
- Integration: system prompt contains `Artifact directory:` line

## Files to change

- `crates/zdx-core/src/config.rs` — add `artifact_root()`, `artifact_dir_for_thread()` to `paths` module
- `crates/zdx-core/src/core/context.rs` — add `artifact_dir` to `PromptTemplateVars`, thread `thread_id` param
- System prompt template (`.md` or inline) — add `Artifact directory:` to `<environment>`
- `crates/zdx-cli/src/cli/commands/imagine.rs` — use `artifact_dir_for_thread` instead of hardcoded path
- Skills: `html-page/SKILL.md`, `imagine/SKILL.md`, `screenshot/SKILL.md`
- Automations: `zdx-daily-interactions-summary.md`, `morning-report.md`
ls: `html-page/SKILL.md`, `imagine/SKILL.md`, `screenshot/SKILL.md`
- Automations: `zdx-daily-interactions-summary.md`, `morning-report.md`
