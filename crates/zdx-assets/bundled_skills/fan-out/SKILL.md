---
name: fan-out
description: "Use when the user wants to send one prompt to several models at once and see every answer — e.g. 'get a second opinion from GPT-5 and Gemini', 'have 3 models review this code', 'compare how different models answer this', 'ask a few models and show me all replies', or 'run this across multiple models'. Dispatches the same prompt to N models in parallel via `zdx exec` and returns all answers with a per-model thread id for follow-up. Not for single-model calls."
---

# Fan-out

Send one prompt to several models in parallel and return all their answers together. This is the "panel of models" primitive: one prompt in, N answers out.

This skill is loaded into the **calling agent's** context — it is *not* a subagent. The agent runs the `zdx exec` calls itself via `bash`, then composes the reply. No subagent hop.

## When to use
- Multiple perspectives on one question: code review from several models, a second opinion, "what would 3 models say about X".
- Comparing model behavior on the same input.
- Any "ask model A and model B (and C) the same thing, show me all of it".

Do **not** use for:
- A single-model call — just run `zdx exec -m <model>` directly.

## 1. Pick the models
Discover valid `-m` ids (these print in the exact `provider:model` form to pass to `-m`):

```
zdx models list                       # enabled providers only
zdx models list --provider openai     # filter to one provider
zdx models list --json                # machine-readable (id, provider, pricing, ...)
```

Use the models the user named. If they didn't name any, pick a small sensible spread (e.g. one strong model per major provider they have enabled) and say which you chose. Never invent ids — take them from `zdx models list`.

## 2. Build the prompt
By default each model runs with the full ZDX system prompt and project context (`AGENTS.md`, memory, skills), so it answers as a context-aware agent. Still make the prompt self-contained for the task: fold in what you want done, any source text/code the user pasted, the audience, and the exact output shape. Gather the real material first (read the file the user referenced, the prior message, etc.) and put it inside `-p`.

If you pass `--no-system-prompt` (clean/isolated mode), the model sees **only** your prompt — then it must carry every bit of context it needs.

For prompts with quotes/newlines, write the prompt to `$ZDX_ARTIFACT_DIR/tmp/` and pass it via `-p "$(cat <file>)"` to avoid shell-escaping issues.

## 3. Fan out in parallel
Run the bundled script — it assigns each model its own resumable thread id (`fanout-<ts>-<slug>`), runs them concurrently, and prints the index-first result. Don't rewrite this by hand; just call it.

```
python3 scripts/fanout.py -p "$(cat "$ZDX_ARTIFACT_DIR/tmp/fanout-prompt.txt")" \
    -m claude-cli:claude-opus-4-8 -m openai:gpt-5.5 -m gemini:gemini-3-pro-preview
```

Options:
- `-m PROVIDER:MODEL` — repeat per model (ids from `zdx models list`).
- `-p "..."` or `--prompt-file FILE` — the self-contained prompt.
- `--with-tools` — let models explore/read files (default: `--no-tools`).
- `--no-system-prompt` — clean/isolated run, no ZDX context (default: full context on).
- `-t LEVEL` — thinking level (default `off`); `--prefix` — thread-id prefix.

The script runs all models in parallel (cost ≈ one call, not N), persists each under a known thread id, parses `zdx exec` output directly (no `jq`), and emits the index-first block described below. It prints per-model `(ERROR)` markers instead of failing the whole run.

## 4. Output shape
The script already emits one index-first block — an index (model → thread id + follow-up recipe) followed by each answer inline:

```
Models: 3 · continue any with: zdx --thread <id> exec -m <model> -p "..."

- claude-cli:claude-opus-4-8 -> fanout-<ts>-claude-cli-claude-opus-4-8
- openai:gpt-5.5 -> fanout-<ts>-openai-gpt-5-5
- gemini:gemini-3-pro-preview -> fanout-<ts>-gemini-3-pro-preview

--- claude-cli:claude-opus-4-8 ---
<answer>
...
```

Relay it to the user as-is, or summarize/compare on top of it. The index is first on purpose: if the combined output is large, ZDX's `bash` tool truncates head-first and saves the full text to `stdout_file` (page it with `Read`), so the index always survives.

## Follow-ups
Any model's thread is resumable — send a follow-up to the same id:

```
zdx --thread fanout-<ts>-<slug> exec -m <model> \
    -t off --no-tools \
    --filter assistant_completed -p "<follow-up question>" | jq -r .text
```

Use the same `-m` and flags as the original run so the continuation stays consistent.
