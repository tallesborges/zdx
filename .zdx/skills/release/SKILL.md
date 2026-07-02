---
name: release
description: Cut a new zdx release. Use when the user asks to release, cut a release, ship a version, publish a build, bump the version, or generate release notes / changelog for zdx. Orchestrates version bump, changelog, and dispatching the manual GitHub Release workflow.
---

# Release zdx

zdx releases are **manual and intentional** — cut one when a set of features/fixes is ready, not on every commit. The mechanical build+publish lives in `.github/workflows/release.yml` (workflow_dispatch only). This skill is the human-facing flow that wraps it: pick a version, write a changelog, dispatch, verify.

## Source of truth

- Version: `crates/zdx-cli/Cargo.toml` → `[package] version`.
- Tag convention: `v<version>` (e.g. version `0.4.0` → tag `v0.4.0`).
- Binaries built by the workflow: `aarch64-apple-darwin` (macOS), `x86_64-unknown-linux-gnu`.

## Flow

### 1. Review what changed since the last release

```sh
git fetch --tags
last=$(git tag --sort=-v:refname | head -1)
echo "Last release: ${last:-<none>}"
git log --oneline ${last:+$last..}HEAD
```

Group commits into a short changelog (Features / Fixes / Other). Prefer the user's framing of "what shipped" over raw commit subjects. If nothing meaningful changed, say so and stop.

### 2. Pick the next version

Semver on `crates/zdx-cli/Cargo.toml` (currently pre-1.0, so):
- Breaking / big feature set → bump **minor** (`0.3.0` → `0.4.0`).
- Small fixes only → bump **patch** (`0.3.0` → `0.3.1`).

Confirm the version with the user if unsure.

### 3. Bump the version and commit

Edit `crates/zdx-cli/Cargo.toml`, then keep the lockfile in sync and verify:

```sh
cargo update -p zdx --precise <new-version>   # refresh Cargo.lock entry
just ci-fast
```

Commit only the version bump:

```sh
git add crates/zdx-cli/Cargo.toml Cargo.lock
git commit -m "release: v<new-version>"
```

Push to master (ask first — pushing is remote-touching):

```sh
git push origin master
```

### 4. Dispatch the release workflow

The workflow tags the current `master` HEAD, builds the binaries, and publishes a GitHub Release with auto-generated notes.

```sh
gh workflow run release.yml -f tag=v<new-version>
# add -f prerelease=true for a pre-release
```

Watch it:

```sh
gh run watch "$(gh run list --workflow=release.yml --limit 1 --json databaseId -q '.[0].databaseId')"
```

### 5. Enrich the changelog (optional)

`--generate-notes` gives a baseline. To replace it with the curated changelog from step 1:

```sh
gh release edit v<new-version> --notes "<curated changelog markdown>"
```

### 6. Verify

```sh
gh release view v<new-version>
```

Confirm both `.tar.gz` assets are attached and the tag points at the intended commit. Report the release URL back to the user.

## Notes & future extensions

- The workflow is `workflow_dispatch` only by design — no automatic per-commit or nightly releases. Keep it that way unless the user explicitly wants automation.
- Possible future add-ons (not built yet): auto-post a release announcement (e.g. Twitter/X, Telegram), richer templated changelogs, Linux arm64 / Intel-mac / Windows targets, `cargo-binstall` metadata.
- The token is the built-in `GITHUB_TOKEN`; no secrets needed for the build+publish itself.
