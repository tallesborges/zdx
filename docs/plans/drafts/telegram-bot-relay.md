> Stage: drafts. Keep this plan current while working: when a scope item is finished, check its box `[ ]`→`[x]`; when the phase's ✅ demo passes, mark the phase done (with date). The plan file is the source of truth, not memory.

# Goals
- Let one ZDX bot delegate a prompt to the other through Telegram and show the target's answer in a dedicated relay chat.
- Keep each bot's normal user-facing group isolated, so human messages never make both bots respond.
- Make every peer interaction explicit, trusted, and bounded to one target run with no reply loop.

# Non-goals
- Putting both bots in either bot's normal project group.
- Accepting human prompts directly in the relay chat.
- Feeding the target's answer back into the source agent turn.
- Autonomous conversations, multi-hop delegation, shared threads, or support for more than one configured peer per bot.
- Exposing native commands such as `/restart`, `/model`, or `/worktree` to peer bots.

# Current state
- Telegram Bot-to-Bot Communication Mode is enabled for both configured peer bots.
- Each bot runs in its own group. The original bot was removed from the other bot's group after both bots responded to the same human messages.
- ZDX currently rejects bot senders in both the early queue gate and ingest authorization.
- `zdx telegram send-message` already provides the outbound transport, and normal bot responses already reply to the incoming Telegram message.

# Phase 1 - One-way delegation through a peer-only relay
- [ ] Create a dedicated non-forum `ZDX Relay` supergroup containing both bots. Keep Group Privacy Mode enabled and avoid admin rights because delivery uses an explicitly addressed command.
- [ ] Add one optional peer configuration per bot in `crates/zdx-engine/src/config.rs`: trusted peer bot ID, peer username, and relay chat ID. Empty configuration keeps all current behavior unchanged.
- [ ] Resolve the receiving bot's own ID and username once with `getMe` when peer mode is configured. Fail startup when the configured identity cannot be validated.
- [ ] Add one shared peer-admission policy used by both `crates/zdx-bot/src/bot/queue.rs` and `crates/zdx-bot/src/ingest/mod.rs`. Accept only an exact `/delegate@ThisBot <non-empty prompt>` from the configured peer ID in the configured relay chat.
- [ ] Ignore every human message in the relay, plus unknown bots, generic mentions, direct replies, wrong targets, and target responses. Outside the relay, preserve the existing human and chat allowlist behavior exactly.
- [ ] Treat the extracted delegation payload only as an agent prompt. Reject slash-prefixed payloads and never route peer input through native command handlers.
- [ ] Bound the relay to one active delegation per peer. A concurrent request receives a short busy response instead of entering the existing unbounded topic queue.
- [ ] Extend the Telegram runtime instruction layer with the configured peer target and reuse `zdx telegram send-message --parse-mode plain` when the user explicitly asks the source bot to delegate. Send `/delegate@TargetBot <prompt>` to the configured relay chat.
- [ ] Add focused tests for exact peer acceptance, wrong peer/chat/target, ignored humans, generic mentions, direct replies, native-command payloads, concurrent requests, unchanged normal-group behavior, and no response loop.
- [ ] Update `docs/SPEC.md` and regenerate the default config template through the existing config workflow.

✅ **Demo**: In the source bot's normal group, ask it to delegate a directory-inspection task to the target bot. Exactly one `/delegate@TargetBot ...` request appears in `ZDX Relay`; the target bot runs once and replies there; the source bot ignores that reply. A human message typed directly in the relay produces no bot response, and a second delegation while the target bot is busy receives a bounded busy result.

# Later
- Inject correlated target results back into the source turn after relay-only dogfooding proves one-way delegation useful.
- Add multiple named peers when a third bot exists.
- Add timeouts, cancellation, and persisted request history when delegations need operational recovery.
- Add autonomous multi-step conversations only with an explicit depth budget and measured need.

# Open questions
- Choose the relay chat ID during implementation; `ZDX Relay` is the proposed group name.
