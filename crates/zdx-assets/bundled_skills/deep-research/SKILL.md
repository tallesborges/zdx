---
name: deep-research
description: Use only when the user explicitly asks for deep research.
---

# Deep Research

Use this skill only when the user explicitly asks for deep research.

This skill is provider-agnostic. Choose the provider or implementation that best fits the user's request and the available environment.

Typical triggers:

- Market or competitor research
- Broad landscape scans
- "Do deep research on X"
- Requests for a long-form, cited report
- Questions that need multi-hop web exploration across many sources

Do not use this skill for:

- Simple current-event lookups
- Fetching one known page
- Narrow factual questions that `web_search` can answer quickly
- Tasks that mainly need repo/file analysis instead of web research

## Default approach

If the user does not specify a provider, use the bundled default implementation in `scripts/parallel_deep_research.py`.

The bundled script:

- creates a deep research run
- uses the default fast/cheap configuration
- polls until completion
- prints the final result to stdout
- can save raw JSON and report artifacts under `$ZDX_ARTIFACT_DIR`

If the user explicitly wants another provider for deep research, follow that provider instead of the bundled default implementation.

## Recommended workflow

1. Decide if deep research is actually warranted.
2. If yes, choose the provider/implementation.
3. Write a strong research prompt.
4. Run the deep research flow.
5. Read the result.
6. Summarize the findings for the user, keeping key citations/limitations.

## Prompt writing guidance

Good prompts are specific about:

- topic and scope
- geography/timeframe
- what comparisons matter
- desired output style
- what to emphasize or exclude

Good example:

Create a research report on the current landscape of developer-focused AI coding agents in 2026. Compare product positioning, core workflows, pricing signals, platform support, and notable technical differentiators. Focus on official product pages, docs, benchmark posts, and credible reporting. End with a concise competitive summary.

Weak example:

Research AI coding tools.

## Command patterns

### Default usage

```bash
python3 scripts/parallel_deep_research.py \
  --save-artifacts \
  -- "Create a research report on the current landscape of developer-focused AI coding agents in 2026."
```

The bundled default implementation currently uses:

- processor: `pro-fast`
- output mode: `text`
- polling interval: 5 seconds

Always prefer the `-fast` variant when using these processors.

Do not switch away from `pro-fast` unless the user explicitly asks.

Keep the default implementation simple unless the user explicitly asks for a different provider or a higher-quality/slower run.

## Processor guidance

Use these as mental guidance only. The bundled default implementation stays on `pro-fast` unless the user explicitly asks to change it.

- `core-fast`: lighter and cheaper; better for more structured or narrower research
- `pro-fast`: default choice; best general option for open-ended deep research
- `ultra-fast`: stronger and more expensive; use only when the user explicitly wants deeper research

Always prefer the fast variant for these processors.

## Artifact guidance

When `$ZDX_ARTIFACT_DIR` is available, prefer `--save-artifacts`. This stores:

- a JSON file with the raw API result
- a markdown/text report when the output contains readable content

Artifacts should stay under `$ZDX_ARTIFACT_DIR/deep-research/`.

## Environment requirements

The script requires:

- `python3`
- `PARALLEL_API_KEY` in the environment

If `PARALLEL_API_KEY` is missing, stop and tell the user exactly that.

## Notes on async strategy

For this skill, use polling by default.

Do not build webhook infrastructure or SSE streaming unless the user explicitly asks for it. This skill is designed for occasional local/manual use, so polling is the simplest and most reliable default.

## Result handling

After the script completes:

- read the returned report carefully
- mention important uncertainty or scope limits
- cite a few notable sources if relevant
- give the user a concise synthesis instead of dumping raw output unless they asked for the full report

If the user wants the full raw result, point them to the saved artifact or return the report directly.