# Goals
- Ship a `/prompt-builder` feature that turns a short natural-language intent into a ready-to-use prompt.
- Insert the generated prompt into the composer instead of auto-sending it.
- Let the user save a generated prompt as a custom command.
- Let the user save a generated prompt as a skill draft.
- Let the user generate prompt variants such as `short`, `strict`, `oracle-heavy`, and `explorer-heavy`.

# Non-goals
- Full workflow execution or stateful looping in MVP.
- Auto-running the generated prompt immediately after creation.
- Replacing custom commands or skills — this feature feeds them.
- Hardcoded prompt families in Rust for every use case.
- Multi-step prompt refinement sessions with persistent history.

# Design principles
- User journey drives order
- Generate first, execute later.
- Insert into composer, do not auto-send.
- Prompt-builder is a launcher for reusable prompting, not a workflow engine.
- Save paths should reuse existing/custom command and skill conventions instead of inventing parallel storage.
- Variants are optional accelerators, not required for the base flow.

# User journey
1. User invokes `/prompt-builder`.
2. ZDX asks for a short intent such as “I just finished the implementation and need a multi-pass Oracle review loop.”
3. ZDX generates a polished prompt and inserts it into the composer.
4. User can edit it, send it, or choose a follow-up action.
5. Optional follow-up actions:
   - save as command
   - save as skill draft
   - generate variants
6. Later, the user can run the saved command or finish turning the draft into a real skill.

# Foundations / Already shipped (✅)

## Command palette + slash command surfaces
- What exists: slash-command palette in TUI, command execution path, and existing built-in commands.
- ✅ Demo: open command palette and run built-ins like `/new`.
- Gaps: no prompt-builder command yet.

## Custom commands foundation
- What exists: custom Markdown commands already load from `$ZDX_HOME/commands/*.md` and `<cwd>/.zdx/commands/*.md`, and they appear in the command palette.
- ✅ Demo: custom commands show in the palette.
- Gaps: command selection still needs composer insertion wiring in later slices of `custom-commands.md`, but the storage/discovery model already exists.

## Skills system
- What exists: file-based skills with YAML frontmatter + markdown body, bundled fallbacks, and discovery from user/project locations.
- ✅ Demo: `ship-first-plan` and `deep-interview` are bundled and load correctly.
- Gaps: no “skill draft” creation flow yet.

## Composer/input insertion path
- What exists: the TUI already supports mutating the input/composer text from UI effects.
- ✅ Demo: built-in command flows already mutate UI state in controlled ways.
- Gaps: no dedicated “insert generated prompt” effect yet.

# Architecture placement
- Prompt generation logic should live in `zdx-engine` or a shared prompt helper layer so multiple surfaces can reuse it.
- The first activation surface should be TUI; Telegram support can come after the core interaction proves useful.
- Saving as command/skill draft should reuse the existing file formats and directories rather than inventing a new artifact system.

# Core concept
- `/prompt-builder` is not a skill and not a workflow.
- It is a user-facing prompt construction tool:
  - input: user intent
  - output: generated prompt text
- The generated prompt can then flow into three destinations:
  1. insert into composer
  2. save as command
  3. save as skill draft

# Output modes

## Base output
- A single polished prompt that matches the user’s intent.
- Default behavior: insert into composer.

## Variants
- Optional generated variants from the same base intent:
  - `short`
  - `strict`
  - `oracle-heavy`
  - `explorer-heavy`
- Variants are generated on demand after the base prompt exists.

## Save targets
- **Save as command**
  - writes a Markdown command file into `$ZDX_HOME/commands/` or `.zdx/commands/`
  - minimal frontmatter: `description`
  - body = generated prompt template
- **Save as skill draft**
  - writes a draft skill file into a draft-friendly location (see Key decisions)
  - includes starter frontmatter (`name`, `description`) and the generated body
  - draft is not automatically bundled or activated as a polished skill

# MVP slices (ship-shaped, demoable)

## Slice 1: TUI prompt-builder command + composer insertion
- **Goal**: User can invoke `/prompt-builder`, describe what they want, and get a generated prompt inserted into the composer.
- **Scope checklist**:
  - [ ] Add `/prompt-builder` built-in command to the command palette
  - [ ] Add a lightweight input flow for the builder request (modal, inline prompt, or dedicated overlay)
  - [ ] Generate a prompt from the user’s intent using a dedicated builder prompt/template
  - [ ] Add a UI effect that inserts the generated prompt into the composer
  - [ ] Do not auto-send the generated prompt
- **✅ Demo**: User runs `/prompt-builder`, types “make me a bug investigation loop with Oracle,” presses Enter, and sees the generated prompt appear in the composer.
- **Risks / failure modes**:
  - Generated prompt is too vague or too long
  - UX may feel clunky if the input flow is too modal or hides too much context

## Slice 2: Save generated prompt as command
- **Goal**: User can take a generated prompt and save it directly as a reusable custom command.
- **Scope checklist**:
  - [ ] Add “Save as command” action after prompt generation
  - [ ] Ask for command name + optional description
  - [ ] Write Markdown command file to `$ZDX_HOME/commands/` by default
  - [ ] Reuse the custom command file format already implemented/planned
  - [ ] Show the saved path in the transcript/status area
- **✅ Demo**: Generate a prompt, choose “Save as command,” name it `review-loop`, and see `$ZDX_HOME/commands/review-loop.md` created.
- **Risks / failure modes**:
  - Name conflicts with built-ins or existing custom commands
  - Saving a giant prompt may create an ugly command file unless formatting is normalized

## Slice 3: Save generated prompt as skill draft
- **Goal**: User can convert a generated prompt into a draft skill file for later refinement.
- **Scope checklist**:
  - [ ] Add “Save as skill draft” action after prompt generation
  - [ ] Ask for draft skill name + description
  - [ ] Write a skill draft file with starter frontmatter + body
  - [ ] Keep draft output separate from polished bundled skills
  - [ ] Clearly label drafts as user-owned unfinished artifacts
- **✅ Demo**: Generate a planning prompt, choose “Save as skill draft,” name it `plan-with-oracle`, and see a draft skill file created.
- **Risks / failure modes**:
  - Users may mistake a draft for a polished production skill
  - Draft location may conflict with discovered live skills if not separated clearly

## Slice 4: Prompt variants
- **Goal**: User can generate alternate prompt variants from the same intent.
- **Scope checklist**:
  - [ ] Add “Generate variants” action after base prompt generation
  - [ ] Support at least 4 variants: `short`, `strict`, `oracle-heavy`, `explorer-heavy`
  - [ ] Let user pick one variant to insert into composer or save
  - [ ] Keep the base prompt available alongside variants
- **✅ Demo**: Generate a base prompt, then request variants and choose `strict` to replace the composer contents.
- **Risks / failure modes**:
  - Variants may be too similar to each other
  - Variant generation may overfit and produce needlessly complex prompts

# Contracts (guardrails)
- `/prompt-builder` never auto-sends the generated prompt.
- Generated prompts remain editable by the user before sending or saving.
- Saving as command must reuse the custom-commands format and location conventions.
- Saving as skill draft must not silently create a live polished bundled skill.
- Variant generation must be explicit and user-triggered.
- Built-in commands still win over saved command names.

# Key decisions (decide early)
- **Activation surface**: start with TUI only; Telegram later if the interaction proves useful.
- **Builder implementation**: use a dedicated internal builder prompt/template rather than hardcoded string assembly.
- **Default destination**: generated prompt goes to the composer, not transcript-only output.
- **Command save location**: default to `$ZDX_HOME/commands/`, with project-local save as a later option.
- **Skill draft location**: use a draft-specific area (for example `$ZDX_HOME/skill-drafts/`) so drafts do not immediately behave like polished live skills.
- **Variant scope**: ship a small curated set first; do not allow arbitrary variant names in MVP.

# Testing
- Manual smoke demos per slice
- Minimal regression tests for:
  - prompt-builder command appears in palette
  - generated prompt inserts into composer
  - save-as-command writes expected Markdown file
  - save-as-skill-draft writes expected draft file
  - variant selection replaces/inserts the chosen prompt correctly

# Polish phases (after MVP)

## Phase 1: Telegram + chat surfaces
- Add prompt-builder interaction to Telegram and other non-TUI surfaces.
- Keep the generated prompt as an editable artifact before send.
- ✅ Check-in demo: build a prompt from Telegram and receive it back ready to reuse.

## Phase 2: Project-local save targets
- Let the user choose between user-global and project-local command save destinations.
- Add project-local skill-draft save path if useful.
- ✅ Check-in demo: save a prompt as `.zdx/commands/review-loop.md` in the current project.

## Phase 3: Richer variants + prompt provenance
- Add more variants (for example `concise`, `teaching`, `debug-heavy`).
- Record which builder intent and variant produced a saved command/draft.
- ✅ Check-in demo: inspect a saved command and see builder provenance metadata.

## Phase 4: Promote-to-command / promote-to-skill from chat history
- Let the user take a manually written prompt from the transcript/composer and save it directly as a command or draft skill.
- ✅ Check-in demo: select a successful prompt from history and turn it into a reusable command without re-running builder.

# Later / Deferred
- Auto-detecting when the user “probably wants prompt-builder” without explicit invocation.
- Full workflow generation from prompt-builder output.
- Sharing commands/skill drafts across repos automatically.
- Auto-curation or ranking of the user’s saved prompts.