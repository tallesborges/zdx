#!/usr/bin/env python3
"""Fan one prompt out to N models in parallel via `zdx exec`, print index-first.

Each model runs in its own resumable thread (`<prefix>-<ts>-<slug>`). Output is
index-first: a header mapping model -> thread id (+ follow-up recipe), then each
model's answer inline. Continue any model with:

    zdx --thread <id> exec -m <model> -p "<follow-up>"

Usage:
    fanout.py -p "PROMPT" -m claude-cli:claude-opus-4-8 -m openai:gpt-5.5
    fanout.py --prompt-file brief.txt -m gemini:gemini-3-pro-preview --no-tools

Discover valid `-m` ids with: `zdx models list [--provider X] [--json]`.
Stdlib only; no third-party deps and no `jq`.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import json
import re
import subprocess
import sys
import time


def slugify(model: str) -> str:
    return re.sub(r"[^A-Za-z0-9]+", "-", model).strip("-")


def run_model(model: str, thread_id: str, prompt: str, thinking: str,
              no_tools: bool, no_system_prompt: bool) -> tuple[str, str, bool, str]:
    """Return (model, thread_id, ok, text_or_error)."""
    cmd = ["zdx", "--thread", thread_id, "exec", "-m", model,
           "-t", thinking, "--filter", "assistant_completed", "-p", prompt]
    if no_tools:
        cmd.append("--no-tools")
    if no_system_prompt:
        cmd.append("--no-system-prompt")

    try:
        proc = subprocess.run(cmd, capture_output=True, text=True)
    except FileNotFoundError:
        return (model, thread_id, False, "`zdx` not found on PATH")

    if proc.returncode != 0:
        err = (proc.stderr or proc.stdout or "").strip()
        return (model, thread_id, False, err or f"exec exited {proc.returncode}")

    # `--filter assistant_completed` emits one JSON object per line, each with a
    # top-level `text`. Take the last one with text (final assistant answer).
    text = None
    for line in proc.stdout.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            obj = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(obj, dict) and isinstance(obj.get("text"), str):
            text = obj["text"]
    if text is None:
        return (model, thread_id, False, "no assistant_completed output")
    return (model, thread_id, True, text.strip())


def main() -> int:
    ap = argparse.ArgumentParser(description="Fan one prompt out to N models in parallel.")
    ap.add_argument("-m", "--model", action="append", default=[], metavar="PROVIDER:MODEL",
                    help="Model id to run (repeatable). Get ids from `zdx models list`.")
    src = ap.add_mutually_exclusive_group(required=True)
    src.add_argument("-p", "--prompt", help="The prompt (self-contained; sub-runs get no other context).")
    src.add_argument("--prompt-file", help="Read the prompt from this file.")
    ap.add_argument("-t", "--thinking", default="off",
                    help="Thinking level for each run (default: off).")
    ap.add_argument("--no-tools", action="store_true",
                    help="Disable tools; clean one-shot answers (default: tools on).")
    ap.add_argument("--no-system-prompt", action="store_true",
                    help="Run clean/isolated with no ZDX system prompt or project "
                         "context (default: full context on).")
    ap.add_argument("--prefix", default="fanout", help="Thread id prefix (default: fanout).")
    args = ap.parse_args()

    if not args.model:
        ap.error("at least one -m/--model is required")

    if args.prompt_file:
        with open(args.prompt_file, "r", encoding="utf-8") as fh:
            prompt = fh.read()
    else:
        prompt = args.prompt
    if not prompt.strip():
        ap.error("prompt is empty")

    # De-dup models, preserve order.
    models: list[str] = []
    for m in args.model:
        if m not in models:
            models.append(m)

    ts = int(time.time())
    jobs = [(m, f"{args.prefix}-{ts}-{slugify(m)}") for m in models]

    with concurrent.futures.ThreadPoolExecutor(max_workers=len(jobs)) as pool:
        futures = {
            pool.submit(run_model, m, tid, prompt, args.thinking,
                        args.no_tools, args.no_system_prompt): m
            for (m, tid) in jobs
        }
        results = {f.result()[0]: f.result() for f in concurrent.futures.as_completed(futures)}

    ordered = [results[m] for (m, _) in jobs]

    # Index-first: header survives head-first truncation, answers follow.
    print(f"Models: {len(ordered)} · continue any with: "
          f'zdx --thread <id> exec -m <model> -p "..."\n')
    for model, tid, ok, _ in ordered:
        flag = "" if ok else "  (ERROR)"
        print(f"- {model} -> {tid}{flag}")
    print()
    for model, _, ok, body in ordered:
        print(f"--- {model} ---")
        print(body if ok else f"ERROR: {body}")
        print()

    return 0


if __name__ == "__main__":
    sys.exit(main())
