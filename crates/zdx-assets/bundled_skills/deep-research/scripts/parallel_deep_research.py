#!/usr/bin/env python3
"""Run Parallel Task API Deep Research with polling-first behavior."""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


API_BASE = "https://api.parallel.ai/v1"
CREATE_RUN_URL = f"{API_BASE}/tasks/runs"
DEFAULT_PROCESSOR = "pro-fast"
DEFAULT_OUTPUT_MODE = "text"
POLL_INTERVAL_SECS = 5.0
REQUEST_TIMEOUT_SECS = 30


class ParallelApiError(Exception):
    def __init__(self, message: str, status_code: int | None = None) -> None:
        super().__init__(message)
        self.status_code = status_code


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Run deep research with the default fast/cheap configuration."
    )
    parser.add_argument(
        "prompt",
        help="Research prompt.",
    )
    parser.add_argument(
        "--save-artifacts",
        action="store_true",
        help="Save raw JSON and report output under $ZDX_ARTIFACT_DIR/deep-research/ if available.",
    )
    return parser.parse_args()


def fail(message: str) -> "NoReturn":
    print(message, file=sys.stderr)
    raise SystemExit(1)


def request_json(
    method: str,
    url: str,
    api_key: str,
    payload: dict[str, Any] | None = None,
    timeout_secs: int = REQUEST_TIMEOUT_SECS,
) -> dict[str, Any]:
    data = None
    headers = {
        "x-api-key": api_key,
        "Content-Type": "application/json",
    }
    if payload is not None:
        data = json.dumps(payload).encode("utf-8")

    request = urllib.request.Request(url, data=data, method=method, headers=headers)
    try:
        with urllib.request.urlopen(request, timeout=timeout_secs) as response:
            body = response.read().decode("utf-8")
            return json.loads(body) if body else {}
    except urllib.error.HTTPError as error:
        body = error.read().decode("utf-8", errors="replace")
        detail = body.strip() or str(error)
        raise ParallelApiError(f"Parallel API HTTP {error.code}: {detail}", error.code)
    except urllib.error.URLError as error:
        raise ParallelApiError(f"Failed to reach Parallel API: {error}")


def build_payload(prompt: str) -> dict[str, Any]:
    payload: dict[str, Any] = {
        "input": prompt,
        "processor": DEFAULT_PROCESSOR,
    }

    if DEFAULT_OUTPUT_MODE == "text":
        payload["task_spec"] = {
            "output_schema": {
                "type": "text",
            }
        }

    return payload


def create_run(api_key: str, prompt: str) -> dict[str, Any]:
    payload = build_payload(prompt)
    return request_json("POST", CREATE_RUN_URL, api_key, payload)


def task_url(run_id: str) -> str:
    quoted = urllib.parse.quote(run_id, safe="")
    return f"{API_BASE}/tasks/runs/{quoted}"


def result_url(run_id: str) -> str:
    quoted = urllib.parse.quote(run_id, safe="")
    return f"{API_BASE}/tasks/runs/{quoted}/result"


def format_task_error(task: dict[str, Any]) -> str:
    error = task.get("error")
    if isinstance(error, dict):
        message = error.get("message")
        if isinstance(message, str) and message.strip():
            return message.strip()
        return json.dumps(error, ensure_ascii=False)
    return json.dumps(task, ensure_ascii=False)


def wait_for_completion(api_key: str, run_id: str) -> None:
    last_status = None
    while True:
        try:
            task = request_json("GET", task_url(run_id), api_key)
        except ParallelApiError as error:
            fail(str(error))

        status = task.get("status")
        if status != last_status:
            print(f"Run status: {status}", file=sys.stderr)
            last_status = status

        if status == "completed":
            return
        if status == "failed":
            fail(f"Deep research failed: {format_task_error(task)}")
        if status == "cancelled":
            fail("Deep research was cancelled")
        if status == "action_required":
            fail("Deep research requires action and this script does not support that flow")

        time.sleep(POLL_INTERVAL_SECS)


def fetch_result(api_key: str, run_id: str) -> dict[str, Any]:
    try:
        return request_json("GET", result_url(run_id), api_key)
    except ParallelApiError as error:
        fail(str(error))


def extract_readable_output(result: dict[str, Any]) -> str:
    output = result.get("output")
    if isinstance(output, dict):
        content = output.get("content")
        if isinstance(content, str) and content.strip():
            return content.strip()
        return json.dumps(output, indent=2, ensure_ascii=False)
    if isinstance(output, str) and output.strip():
        return output.strip()
    return json.dumps(result, indent=2, ensure_ascii=False)


def slugify(value: str) -> str:
    chars = []
    for ch in value.lower():
        if ch.isalnum():
            chars.append(ch)
        elif chars and chars[-1] != "-":
            chars.append("-")
    return "".join(chars).strip("-")[:60] or "deep-research"


def artifact_dir() -> Path | None:
    root = os.environ.get("ZDX_ARTIFACT_DIR", "").strip()
    if not root:
        return None
    return Path(root) / "deep-research"


def save_artifacts(prompt: str, result: dict[str, Any], readable_output: str) -> None:
    target_dir = artifact_dir()
    if target_dir is None:
        print(
            "Skipping artifact save because ZDX_ARTIFACT_DIR is not set.",
            file=sys.stderr,
        )
        return

    target_dir.mkdir(parents=True, exist_ok=True)
    timestamp = datetime.now(timezone.utc).strftime("%Y%m%d-%H%M%S")
    stem = f"{timestamp}-{slugify(prompt)}"

    json_path = target_dir / f"{stem}.json"
    json_path.write_text(json.dumps(result, indent=2, ensure_ascii=False), encoding="utf-8")

    suffix = ".md" if not readable_output.lstrip().startswith("{") else ".txt"
    report_path = target_dir / f"{stem}{suffix}"
    report_path.write_text(readable_output, encoding="utf-8")

    print(f"Saved artifacts:\n- {json_path}\n- {report_path}", file=sys.stderr)


def main() -> None:
    args = parse_args()
    prompt = args.prompt.strip()
    if not prompt:
        fail("Prompt cannot be empty.")

    api_key = os.environ.get("PARALLEL_API_KEY", "").strip()
    if not api_key:
        fail("PARALLEL_API_KEY is not set.")

    try:
        run = create_run(api_key, prompt)
    except ParallelApiError as error:
        fail(str(error))
    run_id = run.get("run_id")
    if not isinstance(run_id, str) or not run_id.strip():
        fail(f"Create-run response did not include a valid run_id: {json.dumps(run)}")

    print(
        f"Created deep research run {run_id} with processor {DEFAULT_PROCESSOR}; polling for completion...",
        file=sys.stderr,
    )
    wait_for_completion(api_key, run_id)
    result = fetch_result(api_key, run_id)
    rendered = extract_readable_output(result)

    if args.save_artifacts:
        save_artifacts(prompt, result, rendered)

    print(rendered)


if __name__ == "__main__":
    main()