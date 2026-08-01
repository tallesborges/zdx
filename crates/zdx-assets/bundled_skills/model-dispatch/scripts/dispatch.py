#!/usr/bin/env python3
"""Dispatch one prompt to explicit model/thinking pairs via `zdx exec`.

Each run gets a resumable thread (`<prefix>-<ts>-<model>-<thinking>[-<subagent>]`).
Output is index-first: a header mapping each run to its thread id, then every
answer inline. Continue any run with:

    zdx --thread <id> exec -m <model> -t <thinking> [--subagent <name>] -p "<follow-up>"

Usage:
    dispatch.py -p "PROMPT" --run claude-cli:claude-opus-4-8
    dispatch.py --prompt-file brief.txt --run claude-cli:claude-opus-4-8@low \
        --run openai:gpt-5.5@high
    dispatch.py -p "PROMPT" --subagent oracle --run openai:gpt-5.5 \
        --run claude-cli:claude-opus-4-8
    dispatch.py -p "PROMPT" --run openai:gpt-5.5@high+oracle \
        --run openai:gpt-5.5@high+explorer

A run is `PROVIDER:MODEL[@LEVEL][+SUBAGENT]`. The thinking level defaults to
medium when omitted. `+SUBAGENT` runs that model under a named subagent role (its
prompt, tools, and defaults); `--subagent` sets the role for runs without a
`+` suffix. Discover valid model IDs with `zdx models list [--provider X] [--json]`.
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

THINKING_LEVELS = {"off", "low", "medium", "high", "xhigh", "max"}


def slugify(value: str) -> str:
    return re.sub(r"[^A-Za-z0-9]+", "-", value).strip("-")


def run_model(model: str, thread_id: str, prompt: str, thinking: str,
              no_tools: bool, no_system_prompt: bool,
              subagent: str | None) -> tuple[str, str, str | None, str, bool, str]:
    """Return (model, thinking, subagent, thread_id, ok, text_or_error)."""
    cmd = ["zdx", "--thread", thread_id, "exec", "-m", model,
           "-t", thinking, "--filter", "assistant_completed", "-p", prompt]
    if subagent:
        cmd += ["--subagent", subagent]
    if no_tools:
        cmd.append("--no-tools")
    if no_system_prompt:
        cmd.append("--no-system-prompt")

    try:
        proc = subprocess.run(cmd, capture_output=True, text=True)
    except FileNotFoundError:
        return (model, thinking, subagent, thread_id, False, "`zdx` not found on PATH")

    if proc.returncode != 0:
        err = (proc.stderr or proc.stdout or "").strip()
        return (model, thinking, subagent, thread_id, False,
                err or f"exec exited {proc.returncode}")

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
        return (model, thinking, subagent, thread_id, False, "no assistant_completed output")
    return (model, thinking, subagent, thread_id, True, text.strip())


def parse_run(value: str) -> tuple[str, str, str | None]:
    """Parse `PROVIDER:MODEL[@LEVEL][+SUBAGENT]` into its three parts."""
    spec, separator, subagent = value.partition("+")
    if separator:
        subagent = subagent.strip()
        if not subagent:
            raise argparse.ArgumentTypeError("run must name a subagent after '+'")
        if "@" in subagent or "+" in subagent:
            raise argparse.ArgumentTypeError(
                f"invalid subagent '{subagent}'; "
                "the run format is PROVIDER:MODEL[@LEVEL][+SUBAGENT]"
            )
    else:
        subagent = None

    model, separator, thinking = spec.rpartition("@")
    if not separator:
        return spec, "medium", subagent
    if not model:
        raise argparse.ArgumentTypeError("run must include a model before @")
    thinking = thinking.lower()
    if thinking not in THINKING_LEVELS:
        levels = ", ".join(sorted(THINKING_LEVELS))
        raise argparse.ArgumentTypeError(
            f"invalid thinking level '{thinking}'; choose one of: {levels}"
        )
    return model, thinking, subagent


def describe(model: str, thinking: str, subagent: str | None) -> str:
    role = f" as {subagent}" if subagent else ""
    return f"{model} @ {thinking}{role}"


def main() -> int:
    ap = argparse.ArgumentParser(description="Dispatch a prompt to model/thinking pairs.")
    ap.add_argument("--run", action="append", type=parse_run, required=True,
                    metavar="PROVIDER:MODEL[@LEVEL][+SUBAGENT]",
                    help="Run specification (repeatable; thinking defaults to medium).")
    src = ap.add_mutually_exclusive_group(required=True)
    src.add_argument("-p", "--prompt", help="The self-contained prompt.")
    src.add_argument("--prompt-file", help="Read the prompt from this file.")
    ap.add_argument("--no-tools", action="store_true",
                    help="Disable tools; tools are enabled by default.")
    ap.add_argument("--no-system-prompt", action="store_true",
                    help="Run without the ZDX system prompt or project context.")
    ap.add_argument("--subagent", metavar="NAME",
                    help="Default subagent for runs without a +SUBAGENT suffix.")
    ap.add_argument("--prefix", default="dispatch", help="Thread id prefix (default: dispatch).")
    args = ap.parse_args()

    if args.prompt_file:
        with open(args.prompt_file, "r", encoding="utf-8") as fh:
            prompt = fh.read()
    else:
        prompt = args.prompt
    if not prompt.strip():
        ap.error("prompt is empty")

    runs: list[tuple[str, str, str | None]] = []
    for model, thinking, subagent in args.run:
        run = (model, thinking, subagent or args.subagent)
        if run not in runs:
            runs.append(run)

    if args.no_system_prompt and any(subagent for _, _, subagent in runs):
        ap.error("subagent runs cannot be combined with --no-system-prompt")

    ts = int(time.time())
    jobs = []
    for model, thinking, subagent in runs:
        role = f"-{slugify(subagent)}" if subagent else ""
        thread_id = f"{args.prefix}-{ts}-{slugify(model)}-{thinking}{role}"
        jobs.append((model, thinking, subagent, thread_id))

    with concurrent.futures.ThreadPoolExecutor(max_workers=len(jobs)) as pool:
        futures = [
            pool.submit(run_model, model, tid, prompt, thinking,
                        args.no_tools, args.no_system_prompt, subagent)
            for model, thinking, subagent, tid in jobs
        ]
        results = {}
        for future in concurrent.futures.as_completed(futures):
            result = future.result()
            results[(result[0], result[1], result[2])] = result

    ordered = [results[(model, thinking, subagent)] for model, thinking, subagent, _ in jobs]

    any_role = any(subagent for _, _, subagent, _ in jobs)
    resume_role = " [--subagent <name>]" if any_role else ""
    print(f"Runs: {len(ordered)} · continue any with: "
          f'zdx --thread <id> exec -m <model> -t <level>{resume_role} -p "..."\n')
    for model, thinking, subagent, tid, ok, _ in ordered:
        flag = "" if ok else "  (ERROR)"
        print(f"- {describe(model, thinking, subagent)} -> {tid}{flag}")
    print()
    for model, thinking, subagent, _, ok, body in ordered:
        print(f"--- {describe(model, thinking, subagent)} ---")
        print(body if ok else f"ERROR: {body}")
        print()

    return 0


if __name__ == "__main__":
    sys.exit(main())
