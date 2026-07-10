---
name: model-dispatch
description: "Use when the user wants another named model to perform a task, or wants to fan out one prompt across models or reasoning levels. Trigger on phrases such as 'ask Opus to review this', 'fan this out', 'compare GPT and Gemini', or 'test this model on low and max'. Supports single-model dispatch, parallel model panels, and paired model/reasoning benchmarks."
---

# Model Dispatch

Dispatch one prompt to one or more explicit models through `zdx exec`. This skill is loaded into the calling agent's context; it is not a subagent hop. Built-in subagents select predefined roles, while model dispatch selects named LLMs.

## When to use
- Send work to one named model: "ask Opus to review this".
- Fan out one prompt to several models and show every answer.
- Compare the same model at multiple thinking levels.
- Compare exact model/thinking pairs, such as Opus at `low` versus GPT at `high`.

The phrase "fan out" must continue to trigger this skill.

## 1. Define the runs

Each run is an explicit `MODEL@LEVEL` pair passed with `--run`. The level is optional and defaults to `medium`.

```text
--run claude-cli:claude-opus-4-8
--run claude-cli:claude-opus-4-8@low
--run openai:gpt-5.5@high
```

Supported levels are `off`, `low`, `medium`, `high`, `xhigh`, and `max`. Providers may map or clamp levels their models do not support exactly; mention this caveat when benchmarking reasoning levels.

Use the models the user named. Otherwise discover valid IDs with `zdx models list` or `zdx models list --json` and choose a small sensible set. Never invent model IDs.

## 2. Build the prompt

By default every run receives the full ZDX system prompt and project context. Still make the task self-contained: include the requested outcome, referenced source material, constraints, audience, and output shape.

Use `--no-system-prompt` only for an intentionally isolated comparison. Use `--no-tools` for clean one-shot answers. For prompts with quotes or newlines, write the prompt under `$ZDX_ARTIFACT_DIR/tmp/` and pass it with `--prompt-file`.

## 3. Dispatch

Use the bundled script rather than reconstructing parallel calls:

```text
python3 scripts/dispatch.py --prompt-file "$ZDX_ARTIFACT_DIR/tmp/dispatch-prompt.txt" \
  --run claude-cli:claude-opus-4-8@low \
  --run openai:gpt-5.5@high
```

Options:
- `--run PROVIDER:MODEL[@LEVEL]` — repeat for each exact run; level defaults to `medium`.
- `-p "..."` or `--prompt-file FILE` — the self-contained prompt.
- `--no-tools` — disable tools; tools are enabled by default.
- `--no-system-prompt` — omit ZDX system and project context.
- `--prefix` — customize the generated thread ID prefix.

The script runs all requested pairs concurrently, gives each a resumable thread ID containing its model and thinking level, and reports individual errors without failing the whole dispatch.

## 4. Return results

For a single run, return the answer and identify the model and thinking level. For comparisons, keep the script's index first, followed by every answer. Do not silently select a winner unless the user asks for evaluation.

Every result is resumable. Continue it with the same thread ID, model, and thinking level:

```text
zdx --thread <id> exec -m <model> -t <level> \
  --filter assistant_completed -p "<follow-up question>"
```
