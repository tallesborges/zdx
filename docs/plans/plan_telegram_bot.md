# Goals
- Provide a `zdx bot` subcommand for Telegram DM-only usage
- Enforce a simple allowlist by Telegram user ID
- Route each DM chat to a single zdx thread and reply to the incoming message

# Non-goals
- Group chats, topics, or real Telegram threads
- Approval/pairing flows
- Streaming edits, reactions, or media handling
- Multi-agent routing

# Design principles
- User journey drives order
- KISS / smallest usable bot
- DM-only + allowlist by default

# User journey
1. Operator configures bot token + allowlist and starts the bot
2. An allowlisted user sends a DM to the bot
3. The bot replies with an agent response in the same chat (reply-to message)

# Foundations / Already shipped (✅)
List capabilities that already exist and should not be rebuilt.

## zdx-core agent runtime
- What exists: UI-agnostic agent loop with providers and tool execution
- ✅ Demo: `zdx exec -p "hello"` returns assistant output
- Gaps: Telegram transport and message mapping

## Thread persistence
- What exists: JSONL thread logs with stable storage paths
- ✅ Demo: existing CLI/TUI threads persist across runs
- Gaps: mapping Telegram `chat_id` to thread ID

## Config loading
- What exists: config path + parsing conventions in zdx-core
- ✅ Demo: `zdx config path` and `zdx config init`
- Gaps: bot-specific config keys (telegram token + allowlist)

# MVP slices (ship-shaped, demoable)
Define Slice 1..N in user-journey order.

## Slice 1: New bot crate + config ✅
- **Goal**: A bot subcommand that loads config and validates Telegram settings
- **Scope checklist**:
  - [x] Add `crates/zdx-bot` and workspace entry
  - [x] Minimal CLI entrypoint (start + exit on config errors)
  - [x] Config schema for `telegram.bot_token` and `telegram.allowlist_user_ids`
- **✅ Demo**: `zdx bot` starts with valid config, fails fast with clear errors when missing
- **Risks / failure modes**:
  - Config path mismatch with existing zdx conventions
  - Token accidentally logged

## Slice 2: Telegram DM intake + allowlist gate ✅
- **Goal**: Receive Telegram updates and accept only allowlisted DM users
- **Scope checklist**:
  - [x] Telegram client in polling mode
  - [x] DM-only filter (ignore group/supergroup/channel)
  - [x] Allowlist check by numeric Telegram user ID
- **✅ Demo**: Allowlisted DM is accepted; non-allowlisted DM is ignored/denied
- **Risks / failure modes**:
  - User ID parsing/format issues
  - Unexpected group messages slipping through

## Slice 3: Agent bridge + reply-to ✅
- **Goal**: Route DM text into zdx-core and send a reply back to Telegram
- **Scope checklist**:
  - [x] Map `chat_id` → zdx thread ID (one thread per DM chat)
  - [x] Build agent request from inbound DM text
  - [x] Post a reply-to message with the agent response
- **✅ Demo**: Allowed DM receives a coherent agent reply in the same chat
- **Risks / failure modes**:
  - Provider misconfiguration causes runtime errors
  - Response length exceeds Telegram limits

## Slice 4: Persistence + shutdown ✅
- **Goal**: Preserve conversation context and exit cleanly
- **Scope checklist**:
  - [x] Persist thread logs per `chat_id`
  - [x] Graceful Ctrl+C handling
- **✅ Demo**: Restarting the bot keeps the DM context
- **Risks / failure modes**:
  - Thread ID collisions
  - Incomplete logs on shutdown

# Contracts (guardrails)
- Respond only to **DMs**
- Respond only to **allowlisted user IDs**
- One zdx thread per Telegram `chat_id`
- Reply-to the incoming message (keeps visual context)
- Do not log bot token or sensitive data

# Key decisions (decide early)
- Telegram library + polling strategy
- Allowlist format (numeric user IDs)
- Thread ID format for `chat_id`

# Testing
- Manual smoke demos per slice
- Minimal regression tests only for contracts

# Polish phases (after MVP)
Group improvements into phases, each with a ✅ check-in demo.
Limited strictly to scope present in Inputs.

## Phase 1: Robust messaging (Not started)
- [ ] Handle Telegram message length limits (chunking)
- [x] Improve error messages for misconfigurations (basic errors implemented)
- ✅ Check-in demo: oversized response is split safely

# Later / Deferred
- Group/topic support (real Telegram threads)
- Approval/pairing flow
- Streaming edits, reactions, media
- Multi-agent routing