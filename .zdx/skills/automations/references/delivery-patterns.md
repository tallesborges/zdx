# Delivery Patterns Reference

Detailed guidance for automation delivery channels, shell reliability, and multi-step delivery flows. Referenced from the main automations SKILL.md.

## General Delivery Principles

### When to add a Delivery block

- Add only when external notification/delivery is explicitly requested or implied.
- Verbs like "notify", "alert", "send me", "text me", "post" imply external delivery.
- If delivery is implied but the target/channel is unclear, ask one focused question:
  > You said "notify me" — should I send via Telegram, WhatsApp, or email, and to which target?

### Delivery block format

```md
## Delivery

- Channel/provider: <telegram | whatsapp | email | slack | ...>
- Target: <chat id / phone / email / webhook / file path>
- Topic/thread policy: <reuse-existing | create-per-run> (if applicable)
- If delivery fails: report the delivery error clearly; run output remains in the automation thread.
```

### Reliability patterns

- Prefer first-party tooling/skills over raw API calls when available.
- For multi-step delivery flows, explicitly capture and reuse IDs from prior steps.
- If one data source fails, continue with available sources unless user requested strict failure.
- Always include a fallback: run output is persisted to the automation thread regardless of delivery success.

## Channel-Specific Guidance

### Telegram

- Prefer dedicated CLI/tool wrappers over raw `curl` when available.
- If topic routing is required, state policy explicitly:
  - **Reuse existing**: reference a known topic ID in the prompt.
  - **Create per run**: include deterministic naming (e.g., `Morning Report - YYYY-MM-DD HH:MM`).
- If creating topics per run, the prompt must:
  1. Create the topic (capture the topic ID).
  2. Send the message to that topic ID.
  3. If topic creation fails, report the error clearly.

Example delivery block:

```md
## Delivery

- Channel: Telegram
- Target: chat ID `<chat_id>`
- Topic policy: create-per-run, name format: `<Report Name> - YYYY-MM-DD HH:MM`
- Steps:
  1. Create topic in the target chat.
  2. Send formatted report to the new topic.
- If topic creation fails: report error; run output remains in automation thread.
- If message send fails: report error with response details.
```

### WhatsApp

- Use the `wacli` skill for sending messages.
- Specify the recipient phone number or group name.
- Keep messages concise — WhatsApp has practical length limits for readability.

Example delivery block:

```md
## Delivery

- Channel: WhatsApp (use `wacli` skill)
- Target: <phone number or group name>
- If delivery fails: report error; run output remains in automation thread.
```

### Email (Gmail)

- Use the `gog` skill for Gmail delivery.
- Specify recipient address and subject line format.
- For recurring emails, include date in subject for easy filtering.

Example delivery block:

```md
## Delivery

- Channel: email (use `gog` skill)
- Target: <email address>
- Subject format: `<Report Name> - YYYY-MM-DD`
- If delivery fails: report error; run output remains in automation thread.
```

## Multi-Channel Delivery

When an automation delivers to multiple channels:

1. Define a primary and fallback channel.
2. Attempt primary first.
3. If primary fails, attempt fallback.
4. Report status for each channel separately.

```md
## Delivery

- Primary: Telegram (chat ID `<id>`, topic: create-per-run)
- Fallback: email via `gog` to <address>
- Attempt primary first. If it fails, attempt fallback.
- Report delivery status for each channel.
- Run output always remains in automation thread regardless of delivery outcome.
```

## Shell Reliability

### Heredocs for multiline content

For multiline strings in shell commands, prefer heredocs over fragile quoting:

```bash
cat <<'EOF'
This is multiline content.
It can contain "quotes" and $variables safely.
EOF
```

### Variable persistence

Do not assume shell variables persist across separate tool invocations. Each tool call runs in its own context. If you need a value from one step in a later step:

- Write it to a temp file, or
- Capture it in the same tool invocation that uses it, or
- Pass it explicitly in the prompt instructions.

### Quoting

- Always quote variables in shell: `"$VAR"` not `$VAR`.
- For JSON payloads in curl, prefer heredocs or a temp file over inline quoting.
- Escape special characters when constructing messages from dynamic content.

## Clarification Questions

When delivery details are ambiguous, ask **one** focused question. Good questions:

- "Should I send via Telegram or email?"
- "What's the Telegram chat ID for delivery?"
- "Should I create a new Telegram topic per run or reuse an existing one?"
- "What email address should receive the report?"

Bad questions (too broad):

- "How should I deliver this?"
- "What are your notification preferences?"
