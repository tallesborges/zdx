---
description: Analyze the repo and create or improve a concise AGENTS.md
---
Analyze this codebase and create (or improve) an `AGENTS.md` at the repo root. This file is loaded into every future agent session in this repo, so it must be concise and contain only what the agent would get wrong without it.

Rules:
- Read the codebase first; do not invent facts that are not in source files.
- Every line must pass: "would removing this make the agent get something wrong?" If no, drop it.
- Do not list every file or directory — the agent can discover those.
- Do not include generic advice ("write clean code", "handle errors", "no secrets in commits").
- Do not invent sections like "Tips for Development" or "Support" unless the source files actually document them.
- Capture environment-specific forbidden commands as first-class content: things that look obvious to run but are wrong in this repo's runtime, sandbox, or dev workflow. These are the highest-value AGENTS.md entries — an agent will get them wrong without you. Infer them from the actual runtime (Dockerfile, devcontainer, sandbox shim, dev-server config, supervisor) rather than guessing.
- For non-trivial repos, draft AGENTS.md and show it to the user before writing to disk so they can correct course in one round.
- If `AGENTS.md` already exists, propose a diff with rationale per change instead of silently overwriting.
- For monorepos / workspaces, suggest a short root `AGENTS.md` plus per-package `AGENTS.md` files, not one giant root file.

Read first — launch Explorer in parallel to survey the codebase and pull back what is actually there. Tell Explorer to read:
- Manifest files: `Cargo.toml`, `package.json`, `pyproject.toml`, `go.mod`, `pom.xml`, `Gemfile`, etc.
- `README.md` and any existing `AGENTS.md` / `CLAUDE.md`
- `justfile` / `Makefile` / `scripts/` and CI workflow files (`.github/workflows/`, `.gitlab-ci.yml`)
- Existing AI tool configs: `.cursor/rules/`, `.cursorrules`, `.github/copilot-instructions.md`, `.windsurfrules`, `.clinerules`
- Container / sandbox configs that constrain runtime behavior: `Dockerfile`, `docker-compose.yml`, `.devcontainer/`, and any custom sandbox or runtime shim referenced from scripts or docs
- For monorepos: top-level manifest plus one manifest per workspace member

Then write `AGENTS.md` with these sections (omit any that do not apply):
1. **Project overview** — what it is, primary language/stack.
2. **Build / run / test / lint commands** — non-obvious ones and how to run a single test. Skip commands obvious from the manifest (e.g. `npm test`, `cargo test`).
3. **Architecture / module map** — only the "big picture" you cannot see from `ls`: data flow, key boundaries, where business logic lives.
4. **Conventions that differ from language defaults** — naming, error handling, imports, formatter choice, anything Cursor/Copilot rules already document.
5. **Repo etiquette** — branch naming, commit style, PR expectations. Only include if found in git log or existing docs.
6. **Deployment / pipelines** — staging vs production targets, container build stages, release flow. Only if the repo actually ships somewhere documented.
7. **Configuration / env vars** — required and meaningful optional vars. Skip vars the manifest or `.env.example` already documents obviously.
8. **Gotchas / runtime constraints / forbidden commands** — sandbox limits, commands that auto-run (and therefore must not be re-run by hand), commands that break the environment, non-obvious setup steps.

Prefix the file with exactly:

```
# AGENTS.md

This file gives coding agents the context they need to work in this repository.
```

At the end of your turn, report:
- The path of the file you wrote or the diff you applied.
- Which sections you included and which you intentionally skipped (with a short reason for each skip).
- Any open questions where the codebase did not give a clear answer.
