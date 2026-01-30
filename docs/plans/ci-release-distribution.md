# CI, Release & Distribution Plan

Ship-first plan for adding CI, automated releases, and distribution (Homebrew + npm) to ZDX.

## Goals
- Automated CI on every PR and push to main
- Fast feedback on code quality (format, lint, tests)
- Build verification across platforms (macOS Intel, macOS ARM, Linux)
- Tag-triggered releases with binaries
- Distribution via Homebrew and npm

## Non-goals
- Windows builds (defer for now)
- Cross-compilation for Linux ARM (defer)
- Code signing / notarization

## Design principles
- User journey drives order
- Start simple, add complexity only when needed
- Codex CLI as reference (same Rust CLI architecture)

## User journey
1. Developer pushes code or opens PR
2. CI runs checks (format, lint, tests)
3. Developer sees green/red status
4. Merge when green
5. Tag release → binaries built → GitHub Release created
6. Users install via `brew install` or `npm install -g`

---

## Foundations / Already shipped (✅)

### Cargo workspace
- What exists: 5 crates (`zdx-cli`, `zdx-core`, `zdx-tui`, `zdx-bot`, `xtask`)
- ✅ Demo: `cargo build --workspace`
- Gaps: none

### Rust toolchain
- What exists: `rust-toolchain.toml` pins 1.90.0 with rustfmt + clippy
- ✅ Demo: `cargo +1.90.0 fmt --check`
- Gaps: none

### Tests
- What exists: `cargo test --workspace --lib --tests --bins`
- ✅ Demo: `cargo test --workspace`
- Gaps: none

---

## MVP slices (ship-shaped, demoable)

### Slice 1: Basic CI (format + lint + test)
- **Goal**: Green/red CI signal on every PR
- **Scope checklist**:
  - [x] Create `.github/workflows/ci.yml`
  - [x] Job: `cargo +nightly fmt --check` (matches AGENTS.md)
  - [x] Job: `cargo clippy --workspace --all-targets --locked -- -D warnings`
  - [x] Job: `cargo test --workspace --lib --tests --bins --locked`
  - [x] Run on `push` (main/master), `pull_request`, `workflow_dispatch`
  - [x] Add `concurrency` to cancel stale runs
  - [x] Add `timeout-minutes` for safety
  - [x] Use `dtolnay/rust-toolchain` for toolchain setup
- **✅ Demo**: Open PR, see checks run, status badge works
- **Risks / failure modes**:
  - Current code may have clippy warnings → fix before merge
  - Tests may be slow → add caching in Slice 2

### Slice 2: Cargo cache
- **Goal**: Faster CI runs (~50-70% time reduction)
- **Scope checklist**:
  - [x] Add `Swatinem/rust-cache@v2` action
  - [x] Cache based on `Cargo.lock` hash
- **✅ Demo**: Second CI run is significantly faster
- **Risks / failure modes**:
  - Cache key collisions (rare, action handles well)

### Slice 3: Multi-platform CI matrix
- **Goal**: Verify builds on macOS (Intel + ARM) and Linux
- **Scope checklist**:
  - [x] Add matrix: `ubuntu-latest`, `macos-14` (ARM), `macos-13` (Intel)
  - [x] Run lint/build/test on all platforms
  - [x] Add `results` aggregator job for single required status check
- **✅ Demo**: PR shows 3 platform jobs, all green
- **Risks / failure modes**:
  - Platform-specific test failures → investigate when they occur
  - macOS runners slower and more expensive

### Slice 4: Change detection (optional optimization)
- **Goal**: Skip CI on docs-only changes
- **Scope checklist**:
  - [ ] Add `changed` job (like Codex CLI)
  - [ ] Detect if only `.md` files changed
  - [ ] Skip build/test jobs if no code changes
- **✅ Demo**: PR touching only README shows skipped jobs
- **Risks / failure modes**:
  - Over-skipping → keep detection conservative

### Slice 5: Release workflow (tag-triggered)
- **Goal**: Push tag → build binaries → create GitHub Release
- **Scope checklist**:
  - [ ] Create `.github/workflows/release.yml`
  - [ ] Trigger on `push: tags: ['v*.*.*']`
  - [ ] Build binary for Linux x86_64 (`x86_64-unknown-linux-gnu`)
  - [ ] Build binary for macOS Intel (`x86_64-apple-darwin`)
  - [ ] Build binary for macOS ARM (`aarch64-apple-darwin`)
  - [ ] Create `.tar.gz` archives with binaries
  - [ ] Upload binaries as GitHub Release assets
  - [ ] Generate release notes from commits
- **✅ Demo**: `git tag v0.1.0 && git push --tags` → Release appears with downloadable binaries
- **Risks / failure modes**:
  - Build times (~15-20 min for all platforms)
  - Tag validation (version in Cargo.toml must match)

### Slice 6: Release caching + optimization
- **Goal**: Faster release builds
- **Scope checklist**:
  - [ ] Add rust-cache to release jobs
  - [ ] Build in `--release` mode with optimizations
  - [ ] Add sccache for compiled object caching
- **✅ Demo**: Release builds complete in <10 min
- **Risks / failure modes**:
  - Cache invalidation on dependency changes

### Slice 7: Homebrew Tap
- **Goal**: `brew tap yourname/zdx && brew install zdx`
- **Scope checklist**:
  - [ ] Create separate repo `homebrew-zdx`
  - [ ] Create `Formula/zdx.rb` with platform detection (Intel + ARM)
  - [ ] Download pre-built binaries from GitHub Release
  - [ ] Auto-update formula on new release (via workflow in main repo)
  - [ ] Calculate SHA256 checksums for each binary
- **✅ Demo**: `brew install yourname/zdx/zdx` works on both Intel and ARM Macs
- **Risks / failure modes**:
  - Need separate repo for tap
  - SHA256 checksums must be updated per release

**Formula structure:**
```ruby
class Zdx < Formula
  desc "Terminal AI assistant"
  homepage "https://github.com/yourname/zdx"
  version "0.1.0"
  license "MIT"
  
  on_macos do
    on_arm do
      url "https://github.com/yourname/zdx/releases/download/v#{version}/zdx-aarch64-apple-darwin.tar.gz"
      sha256 "..."
    end
    on_intel do
      url "https://github.com/yourname/zdx/releases/download/v#{version}/zdx-x86_64-apple-darwin.tar.gz"
      sha256 "..."
    end
  end
  
  on_linux do
    url "https://github.com/yourname/zdx/releases/download/v#{version}/zdx-x86_64-unknown-linux-gnu.tar.gz"
    sha256 "..."
  end
  
  def install
    bin.install "zdx"
  end
  
  test do
    assert_match version.to_s, shell_output("#{bin}/zdx --version")
  end
end
```

### Slice 8: npm Package (Codex-style bundled)
- **Goal**: `npm install -g zdx`
- **Scope checklist**:
  - [ ] Create npm package structure with `vendor/` directory
  - [ ] Create `bin/zdx.js` wrapper that detects platform and runs correct binary
  - [ ] Bundle binaries for all platforms in `vendor/<target>/`
  - [ ] Configure `package.json` with bin entry
  - [ ] Publish to npm registry on release
  - [ ] Add npm publish step to release workflow
- **✅ Demo**: `npm i -g zdx && zdx --version` works
- **Risks / failure modes**:
  - Need npm token for publishing (store as GitHub secret)
  - Package size (~50MB with all binaries)

**npm package structure:**
```
zdx/
├── package.json
├── bin/zdx.js              # Entry point, detects platform
└── vendor/
    ├── aarch64-apple-darwin/zdx      # macOS ARM
    ├── x86_64-apple-darwin/zdx       # macOS Intel
    └── x86_64-unknown-linux-gnu/zdx  # Linux
```

**bin/zdx.js:**
```javascript
#!/usr/bin/env node
const os = require('os');
const path = require('path');
const { execFileSync } = require('child_process');

const platform = os.platform();  // darwin, linux
const arch = os.arch();          // arm64, x64

const targetMap = {
  'darwin-arm64': 'aarch64-apple-darwin',
  'darwin-x64': 'x86_64-apple-darwin',
  'linux-x64': 'x86_64-unknown-linux-gnu',
};

const target = targetMap[`${platform}-${arch}`];
if (!target) {
  console.error(`Unsupported platform: ${platform}-${arch}`);
  process.exit(1);
}

const binary = path.join(__dirname, '..', 'vendor', target, 'zdx');
execFileSync(binary, process.argv.slice(2), { stdio: 'inherit' });
```

---

## Contracts (guardrails)
- CI must run on every PR to `main`
- `cargo +nightly fmt --check` must pass
- `cargo clippy --workspace --all-targets --locked -- -D warnings` must pass
- `cargo test --workspace --lib --tests --bins --locked` must pass
- Release only triggers on `v*.*.*` tags
- All 3 platform binaries must be included in release

## Key decisions (decide early)
- Use `Swatinem/rust-cache` for cargo caching (proven, widely used)
- Use `dtolnay/rust-toolchain` for toolchain setup
- Use nightly for fmt (matches AGENTS.md)
- Single required status check (`results` job pattern from Codex CLI)
- Codex-style bundled npm package (all binaries in vendor/)
- Pin macOS versions explicitly (macos-14 for ARM, macos-13 for Intel)

## Testing
- Manual smoke demos per slice
- No additional test infrastructure needed

---

## Polish phases (after MVP)

### Phase 1: Advanced caching
- Add `sccache` for compiled object caching (like Codex CLI)
- ✅ Check-in demo: sccache stats in job summary

### Phase 2: Dependency auditing
- Add `cargo-deny` for dependency audit
- Add `cargo-shear` for unused deps check
- ✅ Check-in demo: CI catches unused dependency

### Phase 3: Release notes automation
- Auto-generate changelog from conventional commits
- ✅ Check-in demo: Release has formatted changelog

---

## Later / Deferred
- **Windows CI/builds**: Add when Windows users report issues
- **Linux ARM builds**: Add when ARM Linux users need it (Raspberry Pi, AWS Graviton)
- **Code signing**: Only needed for stricter distribution
- **Lightweight npm (optionalDependencies)**: Consider if package size becomes issue

---

## Implementation order

| Slice | Name | Effort | Dependencies |
|-------|------|--------|--------------|
| 1 | Basic CI | 1 day | none |
| 2 | Cargo cache | 30 min | Slice 1 |
| 3 | Multi-platform CI | 1 hour | Slice 1 |
| 4 | Change detection | 1 hour | Slice 3 |
| 5 | Release workflow | 1 day | Slice 3 |
| 6 | Release caching | 30 min | Slice 5 |
| 7 | Homebrew tap | 2-3 hours | Slice 5 |
| 8 | npm package | 1 day | Slice 5 |

---

## References
- [Codex CLI rust-ci.yml](https://github.com/openai/codex/blob/main/.github/workflows/rust-ci.yml)
- [Codex CLI rust-release.yml](https://github.com/openai/codex/blob/main/.github/workflows/rust-release.yml)
- [Sentry: How to publish binaries on npm](https://sentry.engineering/blog/publishing-binaries-on-npm)
- [Homebrew Formula Cookbook](https://docs.brew.sh/Formula-Cookbook)

---

## Installation options (after completion)

```bash
# Homebrew (macOS/Linux)
brew tap yourname/zdx
brew install zdx

# npm (cross-platform)
npm install -g zdx

# Direct download
# → GitHub Releases page
```
