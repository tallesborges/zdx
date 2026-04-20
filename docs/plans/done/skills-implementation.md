# Skills Implementation Plan

## Inputs

- **Project/feature**: Add Agent Skills support to zdx. Skills are folders containing `SKILL.md` files with YAML frontmatter (name, description) and Markdown instructions. At startup, only metadata is loaded; the model uses the `read` tool to load full instructions when a task matches.
- **Existing state**: zdx has hierarchical AGENTS.md loading (`context.rs`), a `read` tool (supports absolute paths), system prompt building, and config infrastructure.
- **Constraints**: Follow [agentskills.io specification](https://agentskills.io/specification). Support existing `~/.codex/skills/` and `~/.claude/skills/` directories for compatibility. Core module must be UI-agnostic (no printing, warnings as data).
- **Success looks like**: User places a skill folder with SKILL.md anywhere in configured paths → zdx discovers it → model sees skill metadata in prompt → model reads full skill when task matches.

---

# Goals

- Discover skills from user and project directories at startup
- Inject skill metadata (name, description, location) into system prompt
- Enable model to activate skills via existing `read` tool
- Maintain compatibility with codex/claude skill directories

# Non-goals

- Skill authoring tools or scaffolding
- Remote skill fetching
- Skill-specific slash commands (e.g., `/skill:name`)
- `allowed-tools` enforcement
- Skills UI in TUI (picker, status display)
- Warning display in TUI (deferred to polish)

# Design principles

- **User journey drives order**: Discovery → prompt injection → model activation
- **Progressive disclosure**: Only metadata at startup; full content loaded on-demand
- **Compatibility over purity**: Load from codex/claude paths even if not spec-compliant
- **Warnings don't block**: Invalid skills emit warnings but don't fail startup
- **UI-agnostic core**: Skills module returns data; never prints directly

# User journey

1. User creates `~/.config/zdx/skills/my-skill/SKILL.md` with frontmatter (name, description) and instructions
2. User starts zdx
3. zdx discovers skill, parses frontmatter, includes in prompt
4. System prompt includes `<available_skills>` XML block with skill metadata
5. User asks a task that matches skill description
6. Model uses `read` tool to load full SKILL.md (absolute path)
7. Model follows skill instructions

# Foundations / Already shipped (✅)

## AGENTS.md hierarchical loading
- **What exists**: `context.rs` loads AGENTS.md from ZDX_HOME, home, ancestors, and project root
- **✅ Demo**: Create `~/AGENTS.md`, run zdx, see content in system prompt
- **Gaps**: None for skills (different loading pattern)

## Read tool with absolute path support
- **What exists**: `read` tool reads files; `test_read_outside_root_allowed` confirms absolute paths work
- **✅ Demo**: Ask model to read an absolute path outside project root
- **Gaps**: None

## Config infrastructure
- **What exists**: `config.rs` with TOML loading, `paths::zdx_home()`
- **✅ Demo**: `zdx config init`, inspect `~/.config/zdx/config.toml`
- **Gaps**: No `[skills]` section yet

## System prompt building
- **What exists**: `build_effective_system_prompt_with_paths()` combines config prompt + AGENTS.md, returns `EffectivePrompt` with warnings
- **✅ Demo**: Set `system_prompt` in config, verify it appears
- **Gaps**: Need to append skills XML and return loaded skills

---

# MVP slices (ship-shaped, demoable)

## Slice 1: Core skills module with multi-source API

- **Goal**: Parse SKILL.md frontmatter and load from a single directory, but design API for multi-source from the start
- **Scope checklist**:
  - [x] Create `crates/zdx-core/src/skills.rs`
  - [x] Define `SkillSource` enum: `ZdxUser`, `ZdxProject`, `CodexUser`, `ClaudeUser`, `ClaudeProject`
  - [x] Define `Skill` struct (name, description, file_path, base_dir, source: `SkillSource`)
  - [x] Define `SkillWarning` struct (skill_path, message)
  - [x] Define `LoadSkillsResult` struct (skills, warnings)
  - [x] Define `LoadSkillsOptions` struct (cwd, enable flags per source) - wired in Slice 2
  - [x] Implement `load_skills_from_dir(dir, source, format)` for recursive format
  - [x] Implement YAML frontmatter parsing (extract `name`, `description`)
  - [x] Handle UTF-8 BOM and CRLF line endings (serde_yaml handles this)
  - [x] Implement `validate_name()` per spec (length ≤64, lowercase alphanumeric + hyphens, no leading/trailing/consecutive hyphens)
  - [x] Implement `validate_description()` per spec (required, length ≤1024)
  - [x] Validate name matches parent directory name
  - [x] Export module from `lib.rs`
  - [x] Add unit tests for parsing, validation, and edge cases
  - [x] Update `AGENTS.md` "Where things are" with new file
- **✅ Demo**:
  1. Create `test-skill/SKILL.md` with valid frontmatter
  2. Write a test that calls `load_skills_from_dir()`
  3. Assert skill is returned with correct name/description/source
  4. `cargo test --lib -p zdx-core skills`
- **Risks / failure modes**:
  - YAML parsing edge cases → use `serde_yaml` (battle-tested)
  - Symlink handling → basic support now, robust dedup in Slice 2

## Slice 2: Multi-source discovery with deduplication

- **Goal**: Load skills from all configured paths with collision handling
- **Scope checklist**:
  - [x] Implement `load_skills(options: LoadSkillsOptions)` that aggregates from all sources
  - [x] Add zdx paths: `~/.config/zdx/skills/`, `.zdx/skills/`
  - [x] Add codex path: `~/.codex/skills/` (recursive format)
  - [x] Add claude paths: `~/.claude/skills/`, `.claude/skills/` (claude format)
  - [x] Implement claude format: one level deep, each subdirectory must contain `SKILL.md`
    - Scan `dir/*/SKILL.md` only (no recursion)
    - Skip entries that aren't directories
    - Skip directories without `SKILL.md`
  - [x] Implement symlink resolution via `canonicalize()` for deduplication
  - [x] Implement name collision detection (first wins, warn about duplicates)
  - [x] Source priority order: zdx-user → zdx-project → codex-user → claude-user → claude-project
  - [x] Add unit tests for multi-source loading and collision handling
- **✅ Demo**:
  1. Create skills in `~/.config/zdx/skills/foo/` and `~/.codex/skills/foo/`
  2. Call `load_skills()` with all sources enabled
  3. Assert only one `foo` skill loaded (from zdx-user), warning emitted for collision
- **Risks / failure modes**:
  - Platform-specific path issues → use `dirs` crate consistently
  - Broken symlinks → skip with warning (don't crash)

## Slice 3: System prompt integration

- **Goal**: Skills appear in model's system prompt
- **Scope checklist**:
  - [x] Implement `format_skills_for_prompt(skills)` returning XML string
  - [x] Implement `escape_xml()` helper (escape `&`, `<`, `>`, `"`, `'`)
  - [x] Update `EffectivePrompt` struct to include `loaded_skills: Vec<Skill>`
  - [x] Update `build_effective_system_prompt_with_paths()` to:
    - Call `load_skills()` with default options (all sources enabled)
    - Append skills XML to system prompt
    - Return loaded skills in `EffectivePrompt`
  - [x] Add skill warnings to `EffectivePrompt.warnings` (convert `SkillWarning` → `ContextWarning`)
- **✅ Demo**:
  1. Create `~/.config/zdx/skills/demo-skill/SKILL.md`:
     ```yaml
     ---
     name: demo-skill
     description: A test skill for demo purposes.
     ---
     # Demo Skill Instructions
     When activated, say "Demo skill activated!"
     ```
  2. Run `zdx` interactively
  3. Ask: "What skills are available?"
  4. Model responds mentioning `demo-skill` with its description
  5. Ask: "Use the demo-skill"
  6. Model calls `read` tool on the skill's absolute path
- **Risks / failure modes**:
  - XML escaping bugs → unit test with special characters (`<`, `&`, quotes)
  - Large skill count bloats prompt → log warning if >20 skills (non-blocking)

## Slice 4: Config options for skill sources

- **Goal**: Users can enable/disable skill sources via config
- **Scope checklist**:
  - [x] Add `SkillsConfig` struct to `config.rs`:
    ```rust
    pub struct SkillsConfig {
        pub enable_zdx_user: bool,      // default: true
        pub enable_zdx_project: bool,   // default: true
        pub enable_codex_user: bool,    // default: true
        pub enable_claude_user: bool,   // default: true
        pub enable_claude_project: bool, // default: true
    }
    ```
  - [x] Add `skills: SkillsConfig` field to `Config` with `#[serde(default)]`
  - [x] Add `[skills]` section to `default_config.toml` with comments
  - [x] Wire config into `build_effective_system_prompt_with_paths()`
  - [x] Add test for config-based source filtering
- **✅ Demo**:
  1. Set `[skills] enable_codex_user = false` in config
  2. Create skill in `~/.codex/skills/test/`
  3. Run zdx, ask about available skills
  4. Verify skill is NOT mentioned
  5. Set `enable_codex_user = true`, verify skill IS mentioned
- **Risks / failure modes**:
  - Config migration for existing users → serde defaults ensure all enabled by default

---

# Contracts (guardrails)

1. **Skills with missing description are skipped** (not loaded, warning emitted)
2. **Name validation follows spec** (length ≤64, lowercase alphanumeric + hyphens, no leading/trailing/consecutive hyphens)
3. **Name must match parent directory** (warning if mismatch, still loaded with warning)
4. **First skill wins on name collision** (later duplicates skipped with warning)
5. **Invalid skills don't crash startup** (warnings only, never panic/error)
6. **Existing AGENTS.md behavior unchanged** (skills appended after AGENTS.md content)
7. **Read tool works with absolute skill paths** (verified: `test_read_outside_root_allowed`)
8. **Core module is UI-agnostic** (no printing, warnings returned as data)

# Key decisions (decide early)

1. **YAML parser**: `serde_yaml` crate (handles BOM, CRLF, edge cases)
2. **Frontmatter delimiter**: Standard `---` fences
3. **Skill path in prompt**: Absolute path (model needs exact location for `read` tool)
4. **Source priority order**: zdx-user → zdx-project → codex-user → claude-user → claude-project
5. **Source enum vs string**: Use `SkillSource` enum for type safety
6. **Claude format definition**: One level deep, scan `dir/*/SKILL.md` only

# Testing

- **Manual smoke demos per slice**: Listed in ✅ Demo sections
- **Unit tests** (in `skills.rs`):
  - `test_valid_skill_loads` - happy path
  - `test_missing_description_skipped` - contract #1
  - `test_invalid_name_warns` - contract #2
  - `test_name_directory_mismatch_warns` - contract #3
  - `test_name_collision_first_wins` - contract #4
  - `test_format_skills_xml_escapes` - XML safety
  - `test_claude_format_one_level_only` - claude format behavior
  - `test_utf8_bom_handled` - encoding edge case
  - `test_broken_symlink_skipped` - robustness

# Polish phases (after MVP)

## Phase 1: Startup feedback
- Log skill discovery summary at info level: "Loaded N skills from M sources"
- Log individual skill warnings at debug level
- Skill warnings flow through `EffectivePrompt.warnings` (existing `ContextWarning` pattern)
- **✅ Check-in**: Run zdx with `RUST_LOG=info`, see skill loading summary

## Phase 2: Filtering
- Add `ignored_skills` glob patterns to config (e.g., `["test-*", "wip-*"]`)
- Add `include_skills` glob patterns to config (empty = all)
- **✅ Check-in**: Add `ignored_skills = ["test-*"]`, verify matching skills excluded

# Later / Deferred

| Item | Trigger to revisit |
|------|-------------------|
| Skill-specific slash commands (`/skill:name`) | User feedback requesting quick activation |
| `allowed-tools` enforcement | Security review / sandboxing work |
| Remote skill URLs | Trust model established |
| Skills picker in TUI | MVP proves skills valuable |
| Skill validation CLI (`zdx skills validate`) | Users author custom skills |
| `metadata` field parsing | Downstream tooling needs it |
| Path redaction in logs | Security audit requires it |
| Skill count limit/truncation | Users report prompt bloat issues |
