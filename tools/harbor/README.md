# Harbor runner for zdx (Terminal-Bench 2.0)

Quickest path to test zdx on Terminal-Bench 2.0 using Harbor.

## Prereqs

- Docker running locally
- `uv` installed
- One provider API key (pick one):
  - `ANTHROPIC_API_KEY`
  - `OPENAI_API_KEY`
  - `OPENROUTER_API_KEY`
  - `GEMINI_API_KEY`

## Install Harbor

```bash
uv tool install harbor
```

## Run a quick smoke test

This uses the custom agent wrapper in `tools/harbor/zdx_agent.py` and installs zdx inside the task container.
Run from the repo root so `tools.harbor.zdx_agent` is importable.

```bash
export ZDX_MODEL="claude-haiku-4-5"
export ZDX_ROOT="/app"

harbor run -d "terminal-bench@2.0" \
  --agent-import-path tools.harbor.zdx_agent:ZdxAgent \
  --agent-kwarg zdx_repo="https://github.com/tallesborges/zdx.git" \
  --jobs-dir "${TMPDIR:-/tmp}/harbor-jobs" \
  -n 1
```

### Optional overrides

- `ZDX_MODEL`: model ID (e.g., `gpt-5.2`, `claude-haiku-4-5`, `gemini-3-flash-preview`)
- `ZDX_THINKING`: zdx thinking level (off|minimal|low|medium|high)
- `ZDX_TOOLS`: comma-separated tool list (e.g., `bash,read,write,edit`)
- `ZDX_ROOT`: task root inside container (default `/app`)
- `ZDX_INSTALL_MODE`: `release` (default) or `source`
- `ZDX_RELEASE_TAG`: release tag to install (default `v0.2.0`)
- `ZDX_RELEASE_URL`: full release asset URL (overrides tag/asset)
- `ZDX_RELEASE_ASSET`: release asset filename (default linux x86_64 tarball)

If the repo is private, use an HTTPS URL with a token or a public mirror.

## Notes

- This uses Harbor's installed-agent interface (installs zdx in the container).
- Output is saved to `/logs/agent/zdx.txt` inside the container for each trial.
- By default it installs the `v0.2.0` release binary for linux x86_64; set
  `ZDX_INSTALL_MODE=source` to build from the repo instead.