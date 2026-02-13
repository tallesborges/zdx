# Skill Installer Overlay

## Goals
- Install skills from configurable GitHub repositories via TUI overlay
- Configure skill repositories in zdx config file
- Browse available skills with installed status indicators
- One-click install from the overlay

## Non-goals
- Skill uninstall/removal (manual for now)
- Skill updates/version management
- Private repo authentication UI (use existing env vars)
- Skill search across multiple repos simultaneously

## Design principles
- User journey drives order
- Reuse existing overlay patterns (model_picker is a good template)
- Keep network I/O in effects, state changes in reducer
- Repository list is config-driven, not hardcoded

## User journey
1. User opens command palette (Ctrl+O)
2. User selects "skills" command
3. Skill picker overlay opens showing configured repos
4. User selects a repo → fetches available skills
5. User sees skill list with (installed) annotations
6. User selects a skill → installs to `~/.zdx/skills/`
7. User sees success message, restarts to pick up skill

## Foundations / Already shipped (✅)

### Overlay System
- What exists: Full overlay infrastructure (mod.rs, OverlayRequest, OverlayUpdate, handle_key pattern)
- ✅ Demo: Open model picker (Ctrl+M) or command palette (Ctrl+O)
- Gaps: None

### Config System
- What exists: TOML config loading, `SkillsConfig` struct
- ✅ Demo: `~/.zdx/config.toml` with `[skills]` section
- Gaps: No `skill_repositories` field yet

### Skills Discovery
- What exists: `skills.rs` with discovery from multiple sources
- ✅ Demo: Skills appear in system prompt
- Gaps: No install capability

### UiEffect Pattern
- What exists: Effect-based async I/O (network, file ops)
- ✅ Demo: `UiEffect::SpawnTokenExchange` for OAuth
- Gaps: None

---

## MVP slices (ship-shaped, demoable)

### Slice 1: Config + Hardcoded Repo List Overlay

- **Goal**: Open a skill picker overlay showing a hardcoded repo, fetch skills list
- **Scope checklist**:
  - [x] Add `skill_repositories: Vec<String>` to config with default `["openai/skills"]`
  - [x] Create `skill_picker.rs` overlay with `SkillPickerState`
  - [x] Add `OverlayRequest::SkillPicker` and `Overlay::SkillPicker` variants
  - [x] Add "skills" command to command palette
  - [x] Add `UiEffect::FetchSkillsList { repo: String }` effect
  - [x] Runtime handler: fetch GitHub API (curl or reqwest), parse JSON
  - [x] Render skill list in overlay (name only, no install status yet)
- **✅ Demo**: Ctrl+O → "skills" → see list of skills from openai/skills repo
- **Risks / failure modes**:
  - GitHub API rate limiting → show error in overlay
  - SSL issues → use reqwest with system certs

### Slice 2: Installed Status + Selection

- **Goal**: Show which skills are already installed, allow selection
- **Scope checklist**:
  - [x] Load installed skills from `~/.zdx/skills/` and `~/.codex/skills/`
  - [x] Annotate skill list items with "(installed)" suffix
  - [x] Add filter/search input (reuse model_picker pattern)
  - [x] Arrow key navigation + Enter to select
  - [x] Show skill description in a detail pane (optional, if space)
- **✅ Demo**: See "(installed)" next to skills you already have, filter by typing
- **Risks / failure modes**:
  - Skill name mismatch between repo and local → normalize names

### Slice 3: Install Selected Skill

- **Goal**: Actually install a skill when user presses Enter
- **Scope checklist**:
  - [x] Add `UiEffect::InstallSkill { repo: String, skill_path: String }` effect
  - [x] Runtime handler: download skill files from GitHub raw URLs
  - [x] Write files to `~/.zdx/skills/<skill-name>/`
  - [x] Show progress/spinner in overlay during install
  - [x] Show success message + "Restart to pick up new skills"
  - [x] Close overlay on success
- **✅ Demo**: Select uninstalled skill → Enter → files appear in ~/.zdx/skills/
- **Risks / failure modes**:
  - Partial download failure → cleanup on error
  - Skill already exists → show warning, don't overwrite

### Slice 4: Multiple Repos in Config

- **Goal**: Support multiple repos, show repo selector or combined view
- **Scope checklist**:
  - [x] Parse `skill_repositories` as list from config
  - [x] Add repo selector in overlay (Tab to switch repos, or nested list)
  - [x] Remember last selected repo in session
  - [x] Default repos: `["openai/skills/skills/.curated", "openai/skills/skills/.system", "anthropics/skills/skills"]`
- **✅ Demo**: Add custom repo to config → see its skills in overlay
- **Risks / failure modes**:
  - Different repo structures → normalize to expect `SKILL.md` in each dir

---

## Contracts (guardrails)
- Never overwrite existing skill directories without explicit confirmation
- Config changes are backward compatible (missing `skill_repositories` = default)
- Network failures show user-friendly error, don't crash
- Overlay closes cleanly on Esc at any state

## Key decisions (decide early)
- **Install location**: `~/.zdx/skills/` (not ~/.codex/skills/) to keep zdx-specific
- **Repo format**: Expect `<owner>/<repo>/path/to/skills-dir` with subdirs containing SKILL.md
- **HTTP client**: Use `reqwest` (already likely in deps) vs shelling out to curl

## Testing
- Manual smoke demos per slice
- Integration test: mock GitHub API response, verify skill files written

## Polish phases (after MVP)

### Phase 1: UX Polish
- Skill descriptions shown on hover/selection
- Keyboard shortcut hint in overlay
- Loading spinner during fetch
- ✅ Check-in demo: Smooth browsing experience with descriptions

### Phase 2: Error Handling
- Retry on transient failures
- Offline mode: show cached skill list
- ✅ Check-in demo: Graceful degradation when offline

## Later / Deferred
- **Skill updates**: Check for newer versions → revisit when users ask
- **Skill removal**: Delete from overlay → revisit after install is solid
- **Private repos**: GitHub token input in UI → use env vars for now
- **Skill search**: Cross-repo search → revisit when >3 repos configured
