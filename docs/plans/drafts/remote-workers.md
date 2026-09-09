# Remote workers for zdx

Status: design proposal, not implemented. Fixed product decisions come from the user; interfaces and defaults below are proposed. Reviewed against the working tree and upstream sources on 2026-09-06. This document does not change `docs/SPEC.md` or the shipped architecture.

## 1. Overview

The problem is capacity, not sandbox isolation. The home Mac is already swapping under concurrent Rust and Android builds. Run a full, normal `zdx exec` agent loop on another host inside a worker-owned checkout, while the orchestrator, Telegram ownership, live local notes, and thread index stay on the Mac. Remote workers hold no bot token. Locally owned worker topics route steering; callbacks retrieve local knowledge through `ask_home` or wait for a human answer with a timeout.

There is no streaming. The remote process is the sole transcript writer. Its JSONL is copied to a read-only local replica on Mini App refresh and once per completed turn. Small control requests and completion notifications remain necessary, but there is no distributed transcript event stream or replay system. Each feature gets a unique branch and worktree, private Cargo target/build directories, and access to one shared sccache daemon per host. Cache reuse is opportunistic, not guaranteed across Rust worktrees.

Decisions this proposal settles:

- Use Tailscale first: ordinary SSH for home-to-host control and file transfer, a private callback HTTP listener on the Mac for host-to-home requests.
- Register long-lived hosts once; create disposable worker workspaces on them. ZDX does not rent VMs or provision cloud infrastructure.
- Use one host-local repository cache and one linked worktree per worker per selected repository. Explicit independent clones cover repositories requiring submodules.
- Pin an immutable base commit before launch. Two features based on the same `main` means the same SHA, not two resolutions of a moving branch.
- Reject a dirty local source by default. Explicit `--dirty ignore` starts from the chosen commit without copying local edits. No automatic stash, commit, patch upload, or push.
- Attach remote eligibility and repository membership to the existing workspace `.zdx/config.toml`, not Telegram identity or a new global project registry.

Daytona/e2b, local-loop remote-tool RPC, peer bots, shared-memory synchronization, and continuous transcript delivery are out of scope. An extra registered macOS host may become available; this is not a bot-to-bot delegation design. Swift/Xcode/iOS tasks require macOS, including on remote hosts.

## 2. Boundaries and communication

```text
                         HOME MAC
 User in worker topic -> local bot -> orchestrator / WorkerManager
                              |               |
                         topic binding    durable worker record
                              |               |
 Mini App -> cached JSONL + local index       | SSH control / snapshot pull
                              ^               v
                              |       REMOTE HOST, no Telegram
                              |       zdx worker-host service
                              |          |             |
                              |       worker A       worker B
                              |       zdx exec       zdx exec
                              |       worktree A     worktree B
                              |       JSONL A        JSONL B
                              |          |             |
                              +---- completion notification

 Remote callback tools ------> private home callback listener
                                   |                 |
                                ask_home          ask_user
                             live local lookup    local topic

 Code return: remote Git commits -> Git fetch/bundle -> local review branch
 Build artifacts: stay remote. Compiler cache: shared on that host only.
```

### Three responsibilities, not two bots

Home owns user intent, host selection, topic routing, permissions, local search, and the imported read-only transcript. The remote host service owns admission, worktree provisioning, child processes, and authoritative execution status. Each `zdx exec` owns the agent loop and its transcript. The service is a small process supervisor, not another orchestrator, agent, or memory service.

Home talks to the host using ordinary OpenSSH over Tailscale. A proposed `zdx worker-host rpc` command forwards one request to the host service's Unix socket and returns one response. Requests carry structured JSON through stdin, not a shell-expanded prompt. SSH also carries context bundles, Git bundles, and transcript snapshots. There is no long-lived SSH stdout stream to interpret as agent events; losing an SSH connection must not terminate the worker.

The host service runs independently under systemd on Linux or launchd on macOS. It launches each exec in its own process group, records the exit status, and terminates/reaps the group on cancellation. Host registration checks matching ZDX build/protocol versions. A mismatch blocks new work instead of introducing a compatibility layer; upgrade only when the host is idle.

The Mac's new callback listener binds only to its tailnet address, on a separate port from the public Mini App server. Tailscale handles NAT traversal and encrypted relay fallback. Restrict tailnet access to home-to-host SSH and host-to-home callback traffic. Use pinned SSH host keys, no agent forwarding, and per-worker callback tokens limited to that worker's operations. Tokens are transferred privately, stored outside transcripts with restrictive permissions, and revoked when the worker is closed. Tailnet membership alone does not authorize arbitrary memory reads or topic posting.

Tailscale supplies stable host naming and enrollment. Tailcat can carry the same traffic using `serve`/`forward`, but its default address dies with its process; persistence requires saved keys and client restrictions. Its upstream stability statement promises neither CLI/API/wire stability nor public-relay uptime. Do not implement a second transport in the first version. A later tailcat adapter changes transport, not worker identity or lifecycle. Neither transport makes a sleeping Mac available. [E1, E2]

## 3. Identity, creation, and steering

### Addressing

- `host_id`: stable configured name such as `build-linux` or `build-mac`.
- `worker_id`: globally unique UUID minted at home, also the ZDX worker thread ID. Names such as `chat-spa` are labels, not routing keys.
- `run_id`: unique ID for one exec turn within the worker. Follow-ups reuse the worker, checkout, branch, and JSONL, but get a new run ID.
- Topic binding: local `(chat_id, topic_id) -> worker_id`; owner/orchestrator thread is stored separately. Remote callbacks contain only worker/run/request IDs, never an arbitrary Telegram destination.
- Repository manifest: local root, remote root, repository identity, immutable base SHA, worker branch, checkout mode, context hashes, and toolchain requirements for every selected repository.

Extend `Create_Thread` with an optional remote execution specification; its existing local behavior remains the default. Other thread-control tools keep addressing the returned thread ID. `Get_Thread_Status` exposes host, run state, base/branch, last contact, and last successful transcript sync rather than a fabricated live tool feed. Proposed CLI equivalents:

```sh
zdx remote host check build-linux
zdx remote start --host build-linux \
  --root ~/projects/example-repo \
  --base origin/main --name chat-spa --prompt 'Implement the chat SPA feature'
zdx remote status WORKER_ID
zdx remote send WORKER_ID --prompt 'Keep the existing chat API unchanged'
zdx remote refresh WORKER_ID
zdx remote cancel WORKER_ID
zdx remote resume WORKER_ID --prompt 'Inspect the interrupted work before continuing'
zdx remote collect WORKER_ID
zdx remote close WORKER_ID
```

All `zdx remote` and `worker-host` commands here are proposed interfaces, not commands available today. `start` returns `worker_id`, `host_id`, resolved bases, branch names, remote cwd, state, and the topic link when launched through a bot owner. Direct CLI starts can omit Telegram, in which case `ask_user` is unavailable with the same no-answer fallback.

### Starting a worker

Host enrollment is one-time operator setup: install Tailscale, OpenSSH, the matching ZDX binary, and required toolchains; install `zdx worker-host serve` as the host's systemd/launchd service; provision its scoped identities and single sccache service; add the home host table and run `host check`. Worker creation reuses that enrolled box. It does not SSH in to install arbitrary dependencies or rent a machine on the user's behalf.

1. Resolve the local workspace's effective config, selected repository member(s), task OS, host, model, and initial prompt. Inspect Git status and resolve the requested base according to section 5. Reserve an ID and persist the launch intent and initial prompt as FIFO item zero at home before contacting the host. Messages arriving during preparation append after it; dispatch stays disabled until readiness.
2. Check host reachability, ZDX version, scoped Git/model credentials, free disk, configured admission limit, and required Rust/Android/Xcode toolchains. Check that the host can call home. Refuse incompatible OS/toolchains before creating a topic or starting builds. Do not silently move to a different host or local execution.
3. Fetch the selected remote branch into the host's repository cache, or receive a Git bundle for an explicitly selected local-only commit. Resolve it to a commit SHA and return that SHA to home. For multiple workers requested from the same base, resolve once and reuse the SHA. For multiple repositories, freeze a separate SHA per member.
4. Persist a remote preparation record keyed by worker ID and manifest hash before filesystem/Git changes. Under a short per-repository provisioning lock, create the unique branch and worktree, then prepare the mirrored context and environment. A retry reconciles the recorded branch, base, and path; an unexpected existing object blocks preparation rather than being overwritten. Mark the workspace ready only after verification. This lock is not held during agent execution or builds.
5. For a bot-owned worker, home creates and persists the mirror-topic binding before sending the first run. If topic creation has an uncertain result, leave the worker prepared and reconcile the topic rather than blindly creating another. No remote process contacts Telegram.
6. Home dispatches FIFO item zero with `start(run_id, prompt, manifest_hash)`. The service durably records acceptance before acknowledging. Repeating the same ID and payload returns its known state, not another exec; the same ID with different content is rejected. Retain each run's payload hash, terminal status, and final snapshot reference until close, not just the latest run. Lost acknowledgement means query this ID, not create a replacement worker.
7. The service starts the ordinary agent loop using the worker-specific home, root, thread, context mode, and environment. The model talks directly to its provider; filesystem tools and builds stay on that host. On exit, the service persists terminal status and an immutable final snapshot before notifying home.

### Steering is queued, not a hidden interrupt

Text in the mirror topic follows the existing home bot routing into `Send_Thread_Message`. While an exec turn is running, home keeps follow-ups in a durable per-worker FIFO; it dispatches the next turn only after the current child is confirmed reaped. An ordinary message does not inject text into a running tool or model request. For urgent changes, cancel, wait for confirmed termination, then send/resume. If the host is unreachable, show `cancel_requested`, not `cancelled`, and do not launch a successor elsewhere.

An answer to an outstanding `ask_user` is different: the local bot routes a reply to that question's ID directly to the blocked callback, not into the next-turn FIFO. Other messages remain queued. One pending human question per worker avoids ambiguous reply routing.

## 4. Two features on one repository

The example deliberately uses the requested `main`. On 2026-09-06, GitHub reports the example repository's default branch as `dev`, while `main` also exists. Do not silently substitute the default branch: a main-based request pins main; a dev-based request pins dev. The pinned SHA is evidence of the check, not a permanent configured pin. [E7]

The proposed default is linked worktrees. Each host maintains an ordinary managed clone used as an object/ref cache, with no agent working in that cache checkout. Do not use `git clone --mirror`: mirror ref updates can overwrite worker branches. The service owns fetch, worktree registration/removal, and repository config; workers own commits on their assigned branches. Refs and repository config are shared, while each worktree has a separate HEAD, index, and files. No worker may reset another branch, change shared remotes, prune worktrees, or run repository maintenance. These are operational rules, not a security sandbox. [E3]

```text
host repository cache: /srv/zdx/repos/example-repo/
                             |
                      main pinned at M
                         /         \
          zdx/WORKER_A/chat-spa     zdx/WORKER_B/config-fix
                commit A1                 commit B1
                commit A2                 commit B2

Each branch starts at M. Neither includes the other's commits.
Each worktree has its own Cargo outputs. Both may build concurrently.
```

Use the first start's returned `base_sha` as the second start's `--base` value. The orchestrator does this automatically for a request to work on both features from the same `main`. Branch names include the full worker UUID; titles alone are not unique. Underlying Git operation, with values supplied by the manifest:

```sh
git -C "$REPO_CACHE" worktree add -b "$WORKER_BRANCH" "$WORKTREE" "$BASE_SHA"
```

The same pattern works on different hosts, except each has its own repository cache and sccache. Git returns the work independently. After A merges, B still rests on M; rebasing/merging B onto the new main is an explicit later task, not an automatic change to a running worker's base.

Full clones do not solve Cargo target sharing: outputs still need private paths. They cost additional repository storage/fetching but avoid shared refs/config and worktree/submodule limitations. Git explicitly warns that multiple superproject checkouts with submodules are not fully supported. Default to `checkout = "worktree"`; a nonempty `.gitmodules` at the selected revision requires explicit `checkout = "clone"` in this first version. Clone mode initializes declared submodules recursively at their pinned commits, after validating every resolved URL against scoped credential permissions. It uses an independent clone, not `--shared` object alternates. Optional/selective submodule provisioning is deferred. This is a specific provisioning choice, not a silent runtime fallback. [E3]

### Cargo: private mutable outputs, shared compiler cache

The user's target-dir concern is correct, with one addition: explicitly make both the target directory and intermediate build directory private. In Cargo shipped with Rust 1.96.0 and 1.97.1, ordinary builds take an exclusive build-directory lock and, when final artifacts are needed, an exclusive artifact-directory lock. Defaults place both in the target tree. A shared `CARGO_TARGET_DIR`, or a separate but shared `build.build-dir`, can serialize same-profile builds. Unique directories remove that cross-worker lock contention, not every source of contention. Shared Cargo registry/cache locks and CPU/RAM/disk pressure still exist. [E4]

For each Rust worker, the launcher sets:

```sh
CARGO_TARGET_DIR="$WORKER_DIR/targets/final"
CARGO_BUILD_BUILD_DIR="$WORKER_DIR/targets/build"
CARGO_INCREMENTAL=0
CARGO_BUILD_JOBS=6
RUSTC_WRAPPER=/srv/zdx/bin/sccache
SCCACHE_DIR=/srv/zdx/cache/sccache
```

These are example paths and a starting job budget, not measured capacity. Use one target/build pair per worker, even for a multi-repo task: Cargo invocations inside that worker may serialize, while different workers do not share these locks. This avoids a cwd-aware wrapper or path-switching convention. Do not let scripts override these paths back to a shared host target. Current zdx and the example repository's `.cargo/config.toml` files do not set target/build directories; environment overrides still need to be controlled. The example repository already disables incremental compilation. [C5]

Run one host-level sccache server, with a bounded cache size and a known shared endpoint. Worker-specific homes must not accidentally start servers competing over the same cache. Disable Rust incremental compilation for cacheable invocations, as sccache requires; accept the tradeoff against repeated worktree-local incremental builds. Linker-invoking crate types such as binaries and proc macros are not cached. [E5]

Important correction: sharing the sccache directory does not guarantee cross-worktree Rust hits. At the inspected sccache revision, Rust cache keys include compiler cwd and most `CARGO_*` environment variables, including path-bearing values. `SCCACHE_BASEDIRS` is used by the C/C++ path, not Rust hashing. The unique cwd/output environment in this design can therefore defeat reuse even for apparently identical builds. Keep cache reuse as an optimization, measure `sccache --show-stats`, and do not promise a warm second Rust worktree. Stable-path namespaces or compiler-wrapper normalization would be a separate optimization, not a reason to share mutable Cargo outputs. [E6]

Host `max_workers` and `cargo_jobs` bound admission and ordinary per-invocation parallelism. They are not hard memory quotas; nested builds and linkers can still oversubscribe. Start with a conservative host budget, measure two real builds, and lower concurrency if needed. Queue additional workers instead of filling another machine's RAM with unbounded jobs. Android similarly gets private project build state and a per-worker Gradle user home initially; share the SDK/NDK installation, not mutable project outputs. sccache is not a Java/Kotlin/Gradle cache. Xcode builds use per-worker DerivedData on macOS.

## 5. Bases, dirty trees, and bringing code home

The source specification is an immutable commit, not a copy of whatever happens to be open on the Mac. The launch receipt always names the requested ref and the actual SHA.

`--base` overrides the selected repository's configured `base`. A bare branch name such as `main` and `origin/main` both mean that branch at the configured repository URL; the managed clone's remote is always named `origin`. Other remote qualifiers are rejected. `HEAD` and `local:refs/heads/feature` explicitly resolve at home. Full commit SHAs are verified as commit objects on the host or transferred from home by bundle. Do not accept arbitrary revision expressions. Record input spelling, resolution source, and SHA. For multi-repo tasks, use each member's configured base unless a member-qualified override is supplied; an unqualified CLI override requires exactly one selected repository.

| Local source state | Launch contract |
| --- | --- |
| Clean, `--base origin/main` | Fetch origin's main, resolve once, pin SHA. Never substitute local HEAD. |
| Dirty: any staged/unstaged tracked change or non-ignored untracked path | Default rejection, with affected paths. Leave index/files unchanged. |
| Dirty, explicit `--dirty ignore` | Start from the requested committed base; list that local changes were excluded. Useful for unrelated local work while two fresh features start from main. |
| Selected committed SHA exists only locally | Transfer a Git bundle into a worker-specific input ref; verify the exact SHA. No upstream push required. |
| User wants uncommitted work included | First create a reviewed commit on a local branch, then start from that commit. MVP has no dirty snapshot mode. Never auto-commit, stash, or indiscriminately include untracked/ignored files. |
| Missing repo/member or non-Git root | Fail preflight; do not initialize a repository or recursively upload a directory. |

Ignored paths are not source input. Git does not classify non-ignored untracked paths as source versus output, so all count as dirty. `--dirty ignore` excludes repository working-tree changes only: explicitly selected out-of-repository AGENTS/skill context is still copied at its current bytes and recorded by hash. For a container, check only selected members and context paths; unrelated dirty iOS work does not block an Android-only launch.

The remote agent leaves ordinary commits on its worker branch when committing was requested; otherwise it reports uncommitted changes for review. Completing an agent turn does not imply a clean tree or permission to publish.

`collect` is a local Git import, not an upstream push: fetch each selected repository's worker branch over the registered SSH connection into a worker-specific local ref, or transfer a Git bundle and fetch that ref. Do not check out, merge, rebase, or overwrite the user's current worktree. Return a per-repository map of remote tip SHA, imported local ref/SHA, and dirty paths; collection is complete only when every selected repository's tip is imported and no working changes remain. Changed submodules need their own collected commit objects before the superproject gitlink can count as collected. Dirty remote changes require an explicit commit task. A later authorized push/PR uses the scoped remote identity. Never push merely because a worker completed.

`close` first confirms no run or accepted cancellation is in flight, verifies every repository's current committed tip has been collected, and checks each checkout for uncommitted/untracked files. Under its worker lock, it then removes only that worker's recorded checkouts, exact worker branches, and disposable execution files after explicit close authorization. Keep a small closed receipt and the collected local refs/transcripts. Uncollected commits or dirty files block cleanup. There is no automatic TTL deleting uncollected code. Cache pruning may remove disposable compiler artifacts, never branches or source worktrees without authorization.

## 6. Mirrored roots and a smaller prompt

Mirror the directory layout relative to the local home, not the literal macOS `/Users/...` prefix. Each worker gets its own home-shaped tree; this keeps ancestor scope and relative references consistent without making concurrent workers share one project path:

```text
/srv/zdx/workers/WORKER_A/
  home/                         HOME for this worker
    .zdx/                       ZDX_HOME for this worker
      AGENTS.md                 synced global instruction file
      config.toml               generated remote-safe config only
      threads/WORKER_A.jsonl     remote-authoritative transcript
    Documents/
      AGENTS.md
      projects/
        work/
          example-client/
            AGENTS.md
            example-repo/      linked worktree, root-is-a-repo
  targets/                      private across all this worker's Cargo invocations
  manifest.json

/srv/zdx/workers/WORKER_B/home/Documents/projects/work/example-client/
  example-client/               root-is-a-container, not git init
    AGENTS.md                   if present at source
    .zdx/skills/                explicitly synced shared workspace skills
    android-client/             selected member's own worktree
```

Worker A and B each have their own ancestor files. Global instructions do not live in a mutable shared folder that changes under active workers. Capture the effective chain at launch using the existing AGENTS/CLAUDE discovery semantics, including `.zdx/AGENTS.md` at each scope and the per-directory fallback. Preserve file contents, order, source labels, and home-relative paths; store both local and remote source paths in the manifest. Root/canonicalization must keep the worktree under its worker home rather than following a symlink out of that ancestry. [C3]

Project-tracked AGENTS files and `.zdx/skills/` come from the selected Git revision. Out-of-repo ancestor instructions and container-shared skills come from the explicit launch context. If selected project instructions are locally edited, the dirty-source decision applies; do not silently overlay the main-based checkout with different project instructions. Validate relative references used during preparation and report missing required context rather than recursively syncing sibling projects.

Absolute local paths inside instructions are not made portable by changing HOME. The remote prompt includes the explicit path mapping and says local-only data must be requested with `ask_home`. A required build dependency at a literal Mac-only path must be fixed or provisioned before that task can start. Do not rewrite instruction contents heuristically or create a universal `/Users` symlink farm. Toolchains, SDKs, and credentials use separately registered host paths.

For root-is-a-container, provision only configured selected repositories and explicitly named shared context paths. An Android worker need not clone the iOS sibling. A cross-repo task can select several members, each with its own branch/base/checkout; all selected members must be supported by the chosen host. An organization root follows the same rule, not a recursive clone of every project. One local project, currently not a Git repo, is ineligible until explicitly initialized or excluded; this proposal does not do that initialization.

Add a remote-worker context mode to the existing exec loop. It keeps ordinary execution, persistence, safety, Git rules, and project instructions, but uses a smaller capability catalog:

- Initial task and relevant supplied context, selected model, real remote OS/toolchain facts, immutable manifest, and synced instruction chain.
- Project/workspace `.zdx/skills/` plus an explicitly portable built-in subset. No automatic user/global skill-directory discovery, including on a remote macOS host. Never copy global `apple-reminders`, `screenshot`, `wacli`, Telegram skills, or other identity-bound integrations.
- No memory index, Notes/Calendar snapshot, local thread history corpus, local SQLite files, Telegram config, automations, or global OAuth cache. Additional knowledge comes through `ask_home`.
- Two callback tools, ordinary remote filesystem/build tools, and only scoped remote credentials. Child subagents inherit the same remote context restrictions and host budget.

The complete global instruction chain is still synced, even when it mentions an unavailable integration. The remote runtime layer explicitly identifies unavailable capabilities and the callback route; it does not pretend those integrations are installed. Existing normal exec always includes memory/skills by default and user skill discovery also depends on HOME, so setting an empty `ZDX_HOME` or `--no-skills` alone is not a complete implementation. [C3]

Construct the worker environment from an allowlist, not a copy of home config or the SSH login shell. Share host-installed toolchains via explicit `CARGO_HOME`, `RUSTUP_HOME`, SDK and PATH settings where suitable, while keeping mutable build outputs private. Provision a separate scoped Git identity per authorized repository set (read-only deploy keys by default, or fine-grained PAT with expiry). Authenticate model access independently on the host or supply a dedicated provider credential; do not copy the Mac's main SSH key, provider OAuth database, or Telegram token. This reduces accidental exposure, but is not a claim of isolation between trusted workers running as the same OS user.

## 7. Configuration

The following TOML is proposed schema. Existing workspace config layering and Telegram `profiles.<name>.cwd` remain the entry points. There is no existing remote schema to reuse. Host inventory belongs only in the home global config; eligibility is workspace-owned. Repository-local config must not be allowed to define home callback credentials or register hosts. [C2]

Home `$ZDX_HOME/config.toml`, illustrative host names and capacity:

```toml
[remote.home]
callback_url = "http://home-mac:4142"

[remote.hosts.build-linux]
ssh = "zdx@build-linux"
root = "/srv/zdx"
os = "linux"
max_workers = 2
cargo_jobs = 6

[remote.hosts.build-mac]
ssh = "zdx@build-mac"
root = "/Users/zdx/remote"
os = "macos"
max_workers = 1
cargo_jobs = 4
```

`callback_url` resolves through MagicDNS; bind its listener to the actual tailnet IP, not all interfaces. Registration provisions a service-owned credential map outside the repository. Each `(host_id, git_identity)` entry names canonical allowed repository URLs, fetch/push capabilities, and a secret-file reference. Validate repository and submodule URLs against this map; knowing an identity name is not a grant to use it elsewhere. SSH identity and host-key configuration stay in the home SSH config. The host reports OS/toolchains during `host check`; a config string alone is not proof of capability.

A single-repo workspace's `.zdx/config.toml`:

```toml
[remote.workspace]
enabled = true
kind = "repo"
hosts = ["build-linux", "build-mac"]

[[remote.workspace.repos]]
name = "example-repo"
path = "."
url = "git@github.com:example-org/example-repo.git"
base = "main"
os = ["linux", "macos"]
checkout = "worktree"
git_identity = "example-read"
```

A container workspace's `.zdx/config.toml`:

```toml
[remote.workspace]
enabled = true
kind = "container"
hosts = ["build-linux", "build-mac"]
context_paths = ["AGENTS.md", ".zdx/skills"]

[[remote.workspace.repos]]
name = "android"
path = "android-client"
url = "https://github.com/example-org/android-client.git"
base = "main"
os = ["linux", "macos"]
checkout = "worktree"
git_identity = "example-read"

[[remote.workspace.repos]]
name = "ios"
path = "ios-client"
url = "git@github.com:example-org/ios-client.git"
base = "develop"
os = ["macos"]
checkout = "worktree"
git_identity = "example-read"
```

Container URLs and base names match the local origin URLs and cached `origin/HEAD` at inspection time; enrollment must recheck them against the origin. The Android member's HTTPS example requires an appropriate scoped token; an SSH deploy-key setup may use the equivalent canonical repository identity. An Android-only launch adds `--repo android`; selecting both members requires macOS. A Swift-only repo uses the single-repo form with `os = ["macos"]`. zdx and other projects use the repo form with their actual origin/default branch, not an assumption that every repo uses `main`. An organization-wide config uses the container form with explicitly enumerated members. A non-Git local project sets `remote.workspace.enabled = false`.

Resolution rules:

Resolve remote settings from raw config layers before the existing recursive TOML merge loses provenance. Reject `remote.home` and `remote.hosts` in non-global layers. This is a proposed config-loader change, not something current deep merging already enforces.

1. The nearest ancestor config declaring a workspace policy owns its root; `path` and `context_paths` resolve relative to that config's project directory, not the caller cwd. A repo's `path = "."` is not reinterpreted as a nested `src/` directory.
2. Treat `remote.workspace` as one policy document from that defining layer, not a deep merge of unrelated container and child policies. Other existing config fields retain current layering semantics. An explicit nearer `enabled = false` disables remote work.
3. Absence means ineligible. Enumerated member paths must be inside the defining root, unique, non-overlapping, and actual Git repositories. No recursive discovery, implicit sibling inclusion, or inherited permission for arbitrary folders.
4. The selected host must be in `hosts`, satisfy every selected repo's OS restriction and the task-specific target requirements, and have matching scoped credentials. Repository permission to use Linux never permits an iOS-specific task on Linux.
5. Origin URL, existence of the configured/requested base branch, submodule requirements, and required context paths are verified before use. A base need not be the upstream default branch. A mismatch fails preflight rather than selecting another repo, base, or checkout mode. The source file, effective values, and resolved paths are recorded in the launch manifest.

Do not sync these home control-plane tables or arbitrary ancestor config files to the worker. Generate the remote execution config from approved portable values and separately supplied credentials. Timeout defaults below are fixed first-version contracts, not additional per-project configuration knobs.

## 8. Callback contracts

Both tools are authenticated, read/reply operations scoped to the current worker. They do not let the remote agent issue home shell commands, write memory, send arbitrary Telegram messages, or recursively delegate another worker.

| Tool | Request and answer | Deadline and fallback |
| --- | --- | --- |
| `ask_home` | Query plus optional note/thread references; returns bounded excerpts, source IDs, and lookup time from live local knowledge. The remote model performs any needed synthesis. | One-second total deadline, with a sub-second warm-path latency target. On `not_found`, `timeout`, or `unavailable`, return that status, not invented facts. Continue only work independent of the missing fact; use `ask_user` for a material decision or finish blocked. |
| `ask_user` | Question, why it blocks, optional choices; home posts in the bound worker topic and correlates a reply using a request ID. | Five-second submission deadline, then up to 15 minutes from accepted submission for an answer. Timeout/unreachable/no topic is not approval. Continue unrelated safe work or finish blocked; never choose a destructive fallback. |

`ask_home` is a fast path owned by the home orchestrator service, not a new full LLM turn queued behind the orchestrator's current work. Use indexed local retrieval and targeted live note reads; refuse/defer cold indexing or unbounded searches on this path. Sub-second is a target, not a guarantee across sleeping hosts, slow local filesystems, or DERP. The one-second deadline is the hard behavior. No remote memory snapshot is retained beyond supplied task context and callback results in its own transcript.

For `ask_user`, create a pending-question record before posting. Subsequent polls use the same request ID and expiry; they do not repost or extend the wait. Expired replies are marked late and are not silently applied as permission. If Telegram accepted a message but its acknowledgement was lost, report delivery as uncertain and await/reconcile a reply rather than blindly posting duplicates. Cancellation expires the question and ends the blocking wait. A bot restart retains pending-question metadata so it can route a reply or explicitly expire it.

Home sleep does not stop an already running remote build. It makes callbacks unavailable, postpones steering, and delays completion delivery. There is no promise that remote work requiring home knowledge can progress while home is offline.

## 9. Transcript refresh and local ownership

The remote worker JSONL is authoritative for execution history. The local file with that worker ID is a replaceable, read-only replica; the local mirror-topic alias is a separate home-owned thread. Store home control metadata outside the remote replica. Never append topic messages, callback replies, synthetic status, or locally rewritten titles into the replica. Remote exec records the prompt/tool results when it actually consumes them. A title change is sent to the remote service and applied only between runs. [C1, C4]

On an explicit Mini App refresh, or a completion notification:

1. Home requests a snapshot for the worker/run. The remote service serializes snapshot creation, opens the thread file once, captures a length, and copies only that prefix ending at the last complete JSONL record. Metadata rewrites are prohibited during active runs. It returns immutable bytes, byte length, checksum, a persistent monotonic per-worker `snapshot_seq`, and execution status separately. This is a persisted checkpoint, not a claim that an in-flight tool has finished.
2. Home serializes imports per worker, downloads into a temporary file, verifies checksum, header/thread association, and every complete JSON record, then atomically renames it into the local thread store. No incremental append, record deduplication, timestamp merge, or transfer-resume protocol. A failed transfer leaves the last valid replica intact.
3. Update only that worker's derived index/export entries and relevant parent/alias relationships. Do not run a full-store reindex, scan all threads, or copy SQLite from remote. A new targeted upsert/import hook is required; ordinary list calls must not pay for remote sync.
4. Return a complete replacement view plus `last_synced_at`, snapshot/run identity, and remote state. Home and browser reject an older `snapshot_seq`; byte length and wall-clock timestamps are not freshness keys. Final snapshots remain addressable by `(run_id, snapshot_seq)` even after later runs. In the browser, remote refresh replaces the whole displayed transcript instead of merging `?after=` deltas.

At turn completion, persist a stable final snapshot before the next run can write. The local FIFO attempts its one automatic import for that run and then can dispatch follow-ups. If notification or transfer fails, retain a pending completion/snapshot reference for later status/refresh or home-start reconciliation; do not restart the agent. Completion notifications are hints keyed by `run_id`, not transcript events. A small delivered marker suppresses repeated normal-path owner wakeups. No exactly-once guarantee is made for Telegram delivery across a crash at its send boundary.

Keep the existing Mini App GET read-only: it displays the local replica without contacting remote hosts. Add an authenticated `POST /api/threads/{id}/refresh` for the explicit remote pull, using the existing Mini App authorization boundary, not callback tokens. Disable automatic live-tool polling for remote replicas; do not synthesize `tool_running` from stale snapshots. Show stale/offline status and preserve the previous content on refresh failure. Current refresh only reloads local data, so this needs both server and frontend work. [C4]

The current Git/Changes view must not execute Git against a remote `root_path` on the Mac or accidentally display the user's unrelated local checkout. Initially mark that pane unavailable for remote threads; code review uses `collect` and a local review branch. A remote Git snapshot view is separate scope. Likewise, a generic local resume/TUI command must refuse to execute an imported remote-owned thread; route through the remote manager or explicitly fork it into a new local thread after collecting code.

Remote child/helper threads are listed in the mutable worker runtime record, separate from the immutable launch manifest, and copied as their own JSONLs during refresh/completion so existing lineage/usage accounting can be retained. Do not scan the whole host's thread store. For the first version, non-source artifact files remain remote and links to them are labeled unavailable locally; use remote inspection or an explicit later artifact download feature. Large truncated tool-output files are included in that limitation. No claim of full artifact synchronization accompanies a transcript refresh.

## 10. State and failures without a distributed replay system

Persist small atomic records with explicit ownership, not duplicate agent event logs:

| Owner | State |
| --- | --- |
| Home | Topic binding, FIFO including initial item zero, dispatch position, pending question/deadline, observed run status, import/notification receipts. |
| Remote workspace | Preparation/manifest identity and `preparing`, `ready`, `closing`, or `closed`. A completed turn does not close its workspace. |
| Remote run | One durable acceptance record per run ID, payload hash, admission/execution status, terminal result, and final snapshot reference. |

The remote service uses a per-worker lock so at most one exec can write its thread at a time. Home sends only one outstanding run per worker; host admission queues accepted runs across workers until a slot is free.

```text
one run: accepted -> admission_queued -> running -> completed | failed | cancelled
                                           |
                                           +-- waiting_for_user (running substate)

next follow-up: a new run ID, same ready workspace, only after the prior child is reaped

Connection state is separate: reachable | unknown/offline.
No contact is not proof of failure, and cancel_requested is not cancelled.
```

- Home/bot restart: reload topic bindings, FIFO, question deadlines, and pending completion receipts; query known remote run IDs. Reattach control, not a second exec. Do not replay a prompt whose acceptance is uncertain.
- Remote exec crash: retain checkout, commits, JSONL checkpoints, and failure status. The normal ZDX checkpoint contract still applies: a crash can lose content since the last tool boundary. An explicit resume inspects the worktree and logs before continuing. Never automatically replay a possibly side-effecting tool. [C1]
- Remote supervisor/host restart: do not auto-restart accepted runs. Reconcile service-owned child identity/termination, mark interrupted runs failed, and require explicit resume after proving no old writer survives. A stored PID alone is insufficient because it can be reused.
- Cancellation: send a request keyed by run ID, clear queued follow-ups as existing cancel does, TERM then KILL/reap the process group. Confirm terminal state before a successor starts. Expire pending callbacks and retain any dirty work.
- Snapshot unavailable or corrupt: keep the previous local snapshot and report its age. Never repair remote JSONL by modifying the local replica and syncing it back.
- Host disk lost: uncollected remote code may be lost. Git collection and normal host storage/backups determine durability; transcript copying is not code backup. There is no live migration or transparent failover in the MVP.

No streaming removes distributed transcript ordering, event replay, and stream reconnection. It does not remove the need to know whether a process started, died, or accepted cancellation. These bounded status/receipt rules are the remaining lifecycle work, not a new distributed workflow engine.

## 11. Implementation boundaries and acceptance

Reuse existing worker IDs, thread-control tools, mirror topics, exec persistence, and process-group cancellation. Add a local/remote runner boundary to WorkerManager, remote host supervision/provisioning, a durable remote-worker record, the two callbacks, remote context selection, and targeted transcript import. The current worker manager's process-lifetime queue and local-root canonicalization cannot be reused unchanged for remote workers. [C1]

When implemented, update `docs/SPEC.md` for remote thread ownership, callback deadlines, queue/cancel semantics, and Mini App refresh; update `docs/ARCHITECTURE.md` for the runner/host/callback boundaries. Update scoped crate instructions and generated default config alongside actual code additions. This proposal leaves those current-state documents intact.

Acceptance checks, not claims of tests already passed:

1. Register one Linux host and run a real Rust task; verify model/tools/build processes are remote while Telegram and memory remain home. Registration reports missing credentials, incompatible OS/toolchains, or version mismatch without starting a worker.
2. Start two features in the same sample repository from the same captured main SHA. Check distinct branches, worktrees, target and build dirs; observe overlapping real builds without cross-worker Cargo output-lock waits. Record wall time, peak RAM, disk, and sccache hit/miss statistics. Do not use a tiny fixture as proof of host capacity.
3. Exercise dirty staged/unstaged/untracked inputs, `--dirty ignore`, a bundled local commit, and a missing member. Confirm no source/index mutation, implicit push, or unrelated-file upload.
4. Start an Android member alone on Linux; preserve shared workspace skills without cloning the iOS sibling. Reject iOS/Swift tasks on Linux. Run a macOS task on a registered macOS host without invoking its peer bot or reading that bot's credentials/state.
5. Inspect the effective remote prompt and skill catalog, including spawned subagents. Confirm the complete instruction chain and path mapping, no local-notes/global Mac skills/token leakage, and functioning bounded live `ask_home` retrieval.
6. Send topic steering during a build, answer an `ask_user`, cancel, and resume. Verify FIFO versus direct question-reply routing, callback expiry, no implicit approval, and no simultaneous writers.
7. Refresh during a long tool, during a write boundary, and at completion. Confirm valid atomic whole-file replacement, stale content on failure, no live polling/remote event stream, targeted indexing, child lineage, and refusal of local execution of the replica.
8. Drop SSH after start acceptance; sleep/wake home; restart both services; interrupt a transfer. Verify no duplicate exec, false cancellation, automatic tool replay, or loss of the last local snapshot. Verify delayed completion can be reconciled.
9. Collect two worker branches into local review refs while the local worktree is dirty. Confirm it is unchanged. Refuse close on dirty/uncollected remote code; clean up only the explicitly closed worker.

No design choice is blocked. Host enrollment, credentials, toolchain installation, measured build capacity/cache benefit, and end-to-end UI/network checks remain deployment/implementation work, not completed work in this document.

## 12. Evidence and provenance

Prior reasoning: internal review transcripts, read for this proposal. Their objections about memory snapshots and transcript streaming are superseded here; separate deployment, credentials, process lifecycle, and Git state still need explicit contracts.

Current code was inspected in the working tree, which already had unrelated edits. References describe observed behavior, not a pristine release:

- C1: `crates/zdx-engine/src/core/workers.rs:800-910`, `crates/zdx-engine/src/core/workers.rs:941-1039`: cancellation, local reattachment, local exec runner, FIFO. `docs/SPEC.md:149-189`: JSONL format, checkpoint durability, atomic metadata rewrite.
- C2: `crates/zdx-engine/src/config.rs:136-204`, `crates/zdx-engine/src/config.rs:330-437`, `crates/zdx-engine/src/config.rs:1057-1091`: Telegram profile shape, ancestor workspace config resolution, and current recursive merging.
- C3: `crates/zdx-engine/src/core/context.rs:724-833`, `crates/zdx-engine/src/core/context.rs:1078-1254`, `crates/zdx-engine/src/skills.rs:410-525`: AGENTS discovery, prompt context, and user/bundled skill loading.
- C4: `crates/zdx-bot/src/server.rs:589-675`, `apps/web/src/views/ThreadView.svelte:39-137`: local thread reads, delta polling, and refresh. `crates/zdx-engine/src/core/thread_index.rs:120-260`: local derived index sync, not remote import.
- C5: `.cargo/config.toml:1-6`, `rust-toolchain.toml:1-3`; the sample repository's `.cargo/config.toml:1-14`. Existing local worktree helper starts at HEAD: `crates/zdx-engine/src/core/worktree.rs:130-177`.

Authoritative external sources, inspected 2026-09-06; GitHub sources inspected through `gh`:

- E1: https://tailscale.com/docs/reference/derp-servers and https://tailscale.com/docs/features/magicdns, NAT traversal/relay and stable device naming.
- E2: https://github.com/tailscale/tailcat/blob/7a50a1abd8bca63432afbcc5c04dd278c7029e31/README.md, `serve`/`forward`, saved keys, client authentication, and Stability sections.
- E3: https://git-scm.com/docs/git-worktree, shared refs/config, per-worktree HEAD/index, branch constraints, and submodule warning.
- E4: https://github.com/rust-lang/cargo/blob/30a34c6821b57de0aaec83a901aca39f88f6778c/src/cargo/core/compiler/layout.rs and https://github.com/rust-lang/cargo/blob/c980f4866141969fab6254a680546a277789d6f0/src/cargo/core/compiler/layout.rs, Cargo pinned by Rust 1.96.0 and 1.97.1 respectively, `Layout::new` locking. https://github.com/rust-lang/cargo/blob/30a34c6821b57de0aaec83a901aca39f88f6778c/src/doc/src/reference/config.md, `build.build-dir` and `CARGO_BUILD_BUILD_DIR`.
- E5: https://github.com/mozilla/sccache/blob/05aafc82b9311edba4747a4656c106512ff181e8/docs/Rust.md and https://github.com/mozilla/sccache/blob/05aafc82b9311edba4747a4656c106512ff181e8/docs/Local.md, incremental/linker limits and single-server local cache.
- E6: https://github.com/mozilla/sccache/blob/05aafc82b9311edba4747a4656c106512ff181e8/src/compiler/rust.rs, `RustHasher::generate_hash_key`, especially cwd and `CARGO_*` hashing. Contrast https://github.com/mozilla/sccache/blob/05aafc82b9311edba4747a4656c106512ff181e8/src/compiler/c.rs for `storage.basedirs()`; generic Configuration.md claims must not be read as a Rust cross-worktree guarantee.
- E7: a sample repository, checked with `gh api`: default branch `dev`, requested `main` exists. Container example values were checked with local `git remote get-url origin` and `git symbolic-ref refs/remotes/origin/HEAD` in each selected repository; those cached branch pointers are not a live upstream-default assertion.
