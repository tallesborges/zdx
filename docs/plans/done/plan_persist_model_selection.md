# Plan: Persist Model Selection

**Feature:** Persist the user's model selection so it survives app restarts.

**Existing state:**
- `Config::load()` reads `~/.config/zdx/config.toml` on startup
- Model picker UI exists (`/model` command) - updates `self.config.model` in memory only
- `Config` struct has `model` field with default `claude-haiku-4-5`
- Terminal safety/restore already implemented (panic hook, Drop, Ctrl+C) ✅

**Constraints:**
- Rust edition 2024, async via tokio
- Config file is TOML format
- No stdout noise while TUI active (SPEC §7)
- Must not break existing config fields

**Success looks like:** User selects a model via `/model`, closes app, reopens → same model is active.

---

# Goals

- Model selection persists across app restarts
- Minimal friction: change takes effect immediately, persists silently
- Works with both default config (no file) and existing config files

# Non-goals

- Persisting other runtime settings (max_tokens, etc.) - out of scope
- Config migration/versioning - not needed for additive change
- Undo/history of model changes

# Design principles

- **User journey drives order**: build the path the user experiences
- **Ship-first**: ugly but working first, polish later
- **Minimal change surface**: one new function, one call site
- **Silent success, visible failure**: persist silently, warn on error in transcript

# User journey

1. User launches `zdx` (model loaded from config or default)
2. User presses `/` → selects `model` → picks a different model
3. Model changes immediately (already works)
4. **NEW:** Model is written to config file in background
5. User quits and relaunches → same model is active

# Foundations / Already shipped (✅)

## Terminal safety/restore
- **What exists:** Panic hook, Drop impl, Ctrl+C handling all restore terminal
- **✅ Demo:** Run `zdx`, Ctrl+C, verify prompt returns clean
- **Gaps:** None

## Config loading
- **What exists:** `Config::load()` → `Config::load_from(path)` with defaults + merge
- **✅ Demo:** `cat ~/.config/zdx/config.toml` shows persisted values
- **Gaps:** No `Config::save()` or partial update

## Model picker UI
- **What exists:** `/model` command opens picker, `execute_model_selection()` updates `self.config.model`
- **✅ Demo:** Run `zdx`, `/model`, select different model, see "Switched to X" message
- **Gaps:** Changes are memory-only, lost on restart

---

# MVP slices (ship-shaped, demoable)

## Slice 1: Persist model on selection

**Goal:** When user selects a model, write it to config file.

**Scope checklist:**
- [ ] Add `Config::save_model(model: &str) -> Result<()>` in `src/config.rs`
  - If config file exists: read, parse, update `model` field, write back
  - If config file doesn't exist: create with just `model = "..."`
  - Preserve other fields and comments (best-effort via toml_edit)
- [ ] Call `Config::save_model()` from `execute_model_selection()` in `src/ui/tui.rs`
- [ ] On error: show warning in transcript (don't block UI)

**✅ Demo:**
1. Delete config: `rm ~/.config/zdx/config.toml`
2. Run `zdx`, `/model`, select "Claude Sonnet 4.5"
3. See "Switched to Claude Sonnet 4.5" in transcript
4. Quit, check: `cat ~/.config/zdx/config.toml` → shows `model = "claude-sonnet-4-5-20250929"`
5. Relaunch `zdx` → status line shows "claude-sonnet-4-5-20250929"

**Failure modes / guardrails:**
- Config file read-only: warn in transcript, don't crash
- Invalid TOML in existing file: warn, don't overwrite (preserve user data)
- Concurrent writes: unlikely (single user), but use atomic write pattern

---

# Contracts (guardrails)

1. **Terminal restore always runs** on exit/panic/Ctrl+C (already shipped)
2. **No stdout noise while TUI active** (errors go to transcript, not stderr)
3. **Config file never silently corrupted** - if parse fails, don't overwrite
4. **Model change is immediate** - UI updates before disk write completes
5. **Graceful degradation** - if save fails, model still works for current session

---

# Key decisions (decide early)

## TOML editing strategy
**Decision:** Use `toml_edit` crate to preserve comments and formatting.
- Alternative: re-serialize entire Config struct (loses comments)
- Risk: toml_edit adds a dependency
- Mitigation: It's a common, well-maintained crate

## Async vs sync write
**Decision:** Sync write in `execute_model_selection()` (blocking).
- Justification: Config writes are rare (<1/session), file is tiny, latency is <1ms
- Alternative: spawn_blocking or async write
- Risk: Blocks event loop briefly
- Mitigation: Acceptable for MVP; optimize if profiling shows issue

## Error handling
**Decision:** Warn in transcript, don't fail.
- Justification: Model selection should feel instant; persistence is best-effort
- Alternative: Show error modal
- Risk: User might not notice warning
- Mitigation: Warning is visible in transcript history

---

# Testing

## Manual smoke test (per slice)
1. Fresh install (no config file) → select model → verify file created
2. Existing config with other fields → select model → verify fields preserved
3. Read-only config → select model → verify warning, no crash
4. Relaunch → verify model loaded

## Automated tests
- [ ] `test_save_model_creates_file` - saves to new file
- [ ] `test_save_model_preserves_other_fields` - updates existing file
- [ ] `test_save_model_handles_missing_dir` - creates parent dirs

---

# Polish phases (after MVP)

## Phase 1: Better error UX
- Show which file failed to write
- Offer "copy to clipboard" for manual fix
- ✅ Check-in: Error message includes path

## Phase 2: Config reload command
- `/config reload` to re-read from disk
- Useful if user edits file externally
- ✅ Check-in: Reload updates model in status line

---

# Later / Deferred

| Item | Trigger to revisit |
|------|-------------------|
| Persist other settings (max_tokens) | User request or UX pain |
| Config file watcher (auto-reload) | Multi-instance use case |
| Config backup before write | Data loss report |
| Async/background persistence | Profiling shows latency issue |

---

# Implementation notes

## `Config::save_model()` sketch

```rust
use toml_edit::{DocumentMut, value};

impl Config {
    /// Saves only the model field to the config file.
    /// Creates the file if it doesn't exist.
    /// Preserves existing fields and comments.
    pub fn save_model(model: &str) -> Result<()> {
        let path = paths::config_path();
        
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        
        // Read existing or start fresh
        let contents = if path.exists() {
            fs::read_to_string(&path)?
        } else {
            String::new()
        };
        
        // Parse as editable document
        let mut doc: DocumentMut = contents.parse()
            .unwrap_or_else(|_| DocumentMut::new());
        
        // Update model field
        doc["model"] = value(model);
        
        // Write atomically (write to temp, rename)
        let tmp_path = path.with_extension("toml.tmp");
        fs::write(&tmp_path, doc.to_string())?;
        fs::rename(&tmp_path, &path)?;
        
        Ok(())
    }
}
```

## Call site in TUI

```rust
fn execute_model_selection(&mut self) {
    // ... existing code to get model_id and display_name ...
    
    self.config.model = model_id.clone();
    self.close_model_picker();
    
    // Persist to config file (best-effort)
    if let Err(e) = Config::save_model(&model_id) {
        self.transcript.push(HistoryCell::system(format!(
            "Warning: Failed to save model preference: {}", e
        )));
    }
    
    self.transcript.push(HistoryCell::system(format!(
        "Switched to {}", display_name
    )));
}
```
