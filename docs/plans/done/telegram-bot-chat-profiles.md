# Goals
- Run one Telegram bot process that can serve multiple project chats.
- Configure each project with only a name, chat ID, and cwd for the first usable version.
- Route each incoming Telegram message to the correct workspace before starting the agent turn.
- Keep the existing single-root bot behavior working while the new profile model is dogfooded.

# Non-goals
- Custom per-profile system prompts or instruction overrides in the MVP.
- Automatic Telegram chat discovery in the MVP.
- UI polish for managing profiles beyond a simple command/config path.
- Changing the existing topic/thread conversation model unless required for cwd routing.

# Design principles
- User journey drives order.
- One bot identity should not imply one workspace.
- Keep the MVP config small: name, chat ID, cwd.
- Preserve existing bot behavior until a profile is configured.

# User journey
1. User creates or chooses a Telegram group for a project.
2. User gets the chat ID for that group.
3. User adds a profile with name, chat ID, and cwd.
4. User sends a message in that group.
5. ZDX runs the agent with that profile cwd, so the right `AGENTS.md` and project files are visible.

# Foundations / Already shipped (✅)

## Telegram bot runtime
- What exists: `crates/zdx-bot` already ingests Telegram messages, extracts chat IDs, dispatches work by chat/topic, and runs agent turns.
- ✅ Demo: `just bot` starts the existing bot flow.
- Gaps: the bot context currently has one root for the whole bot instance.

## Bot config and allowlists
- What exists: `TelegramConfig` in `crates/zdx-engine/src/config.rs` already stores Telegram token, allowlisted users/chats, model, and thinking level.
- ✅ Demo: existing config can allow or reject chats before handling messages.
- Gaps: there is no per-chat cwd map yet.

## Thread/topic routing
- What exists: incoming Telegram messages already carry `chat_id` and optional topic ID, and the bot already derives thread identity from them.
- ✅ Demo: messages in different chats/topics remain separated today.
- Gaps: thread identity is separate from workspace identity; workspace still comes from the single bot root.

# MVP slices (ship-shaped, demoable)

## Slice 1: Config model for chat profiles
- **Goal**: Represent multiple project profiles in config with only name, chat ID, and cwd.
- **Scope checklist**:
  - [ ] Add a `TelegramProfileConfig` type with `chat_id` and `cwd`.
  - [ ] Add `profiles` to `TelegramConfig`, keyed by profile name.
  - [ ] Keep current single-root behavior valid when no profile matches.
  - [ ] Ensure config serialization/deserialization preserves existing config compatibility.
- **✅ Demo**: a config file can define two profiles and load successfully.
- **Risks / failure modes**:
  - Existing bot configs fail to parse.
  - Relative or `~` cwd handling differs from current root handling.

## Slice 2: Runtime profile resolution
- **Goal**: Resolve the active cwd from the incoming Telegram chat before running the agent.
- **Scope checklist**:
  - [ ] Store profiles in `BotContext` or an adjacent resolver.
  - [ ] Resolve `incoming.chat_id` to a profile.
  - [ ] Use the profile cwd when computing the agent worktree root.
  - [ ] Keep existing root fallback for unprofiled chats that are still allowlisted.
  - [ ] Make `/status` show the effective cwd/profile.
- **✅ Demo**: two Telegram chats handled by the same bot run from two different local folders.
- **Risks / failure modes**:
  - Worktree/topic logic accidentally reuses a worktree from another profile.
  - Status/debug output still reports the old single root.

## Slice 3: Simple profile creation command
- **Goal**: Make adding a profile easy without hand-editing TOML.
- **Scope checklist**:
  - [ ] Add or extend a CLI command that accepts `name`, `chat_id`, and `cwd`.
  - [ ] Write into the existing config location using the repo’s config update conventions.
  - [ ] Validate only boundary inputs: missing name, invalid chat ID, nonexistent cwd.
  - [ ] Print the resulting profile summary.
- **✅ Demo**: user runs one command, restarts the bot, and the group uses the configured cwd.
- **Risks / failure modes**:
  - Command edits the wrong config file.
  - Duplicate names or chat IDs create ambiguous routing.

## Slice 4: Regression checks and dogfood path
- **Goal**: Protect the new routing behavior enough to dogfood confidently.
- **Scope checklist**:
  - [ ] Add focused tests for profile config parsing.
  - [ ] Add focused tests for chat ID to cwd resolution.
  - [ ] Add one smoke/manual checklist for running two profiles locally.
  - [ ] Run targeted checks, then `just ci` when ready.
- **✅ Demo**: automated tests pass and manual bot smoke confirms two chats map to two folders.
- **Risks / failure modes**:
  - Tests cover config but not the actual message path.
  - Manual Telegram testing is skipped because chat IDs are unavailable.

# Contracts (guardrails)
- A single bot token/process can serve multiple configured project chats.
- Profile routing is based on Telegram `chat_id` for the MVP.
- Each profile has exactly the user-requested MVP fields: name, chat ID, cwd.
- A configured chat must run the agent from its configured cwd.
- Existing allowlist behavior must not become more permissive by accident.
- Existing unprofiled single-root bot usage should keep working during migration.

# Key decisions (decide early)
- Config location and shape: prefer `telegram.profiles.<name>` unless existing bot-specific config requires a better home.
- Duplicate handling: reject duplicate profile names and duplicate chat IDs.
- Fallback behavior: keep current root fallback for compatibility, but make profile matches take precedence.
- CLI naming: choose one simple command form before implementation to avoid rework.

# Testing
- Manual smoke demos per slice.
- Minimal regression tests only for contracts.
- Prefer focused tests around config loading and profile resolution before broader bot integration tests.
- Run `just ci` as final verification after code changes.

# Polish phases (after MVP)

## Phase 1: Easier onboarding
- Add a helper command or bot command to display the current `chat_id`.
- ✅ Check-in demo: user creates a Telegram group, asks for/gets the chat ID, then adds a profile.

## Phase 2: Per-profile instructions
- Add optional instruction/profile prompt support only after cwd routing is stable.
- ✅ Check-in demo: two profiles use different cwd plus different extra instructions.

## Phase 3: Better management UX
- Add list/update/remove profile commands if hand editing or one-shot add becomes annoying.
- ✅ Check-in demo: user lists profiles and updates a cwd without opening the config file.

# Later / Deferred
- Topic-level profiles: revisit if one Telegram group should contain many projects by topic.
- Per-profile model/tool permissions: revisit after multiple projects are actively dogfooded.
- Fully automated profile creation from inside Telegram: revisit after the CLI command is proven useful.
- Per-profile memory scopes: revisit when personal and project usage start conflicting.