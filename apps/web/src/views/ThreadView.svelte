<script lang="ts">
  import { api, ApiError } from "$lib/api";
  import type {
    ThreadResponse,
    ThreadTrajectoryReport,
    GitResponse,
    WorkerItem,
  } from "$lib/types";
  import { router, THREAD_TAB_LABELS, type ThreadTab } from "$lib/router.svelte";
  import { haptic, openTelegramLink, selectionChanged } from "$lib/telegram";
  import { isBusy, needsAttention, rollupFrom } from "$lib/workers";
  import TabStrip from "../components/TabStrip.svelte";
  import type { IconName } from "../components/Icon.svelte";
  import TranscriptPane from "./TranscriptPane.svelte";
  import ArtifactsPane from "./ArtifactsPane.svelte";
  import ChangesPane from "./ChangesPane.svelte";
  import AgentPane from "./AgentPane.svelte";
  import TrajectoryPane from "./TrajectoryPane.svelte";
  import WorkersPane from "./WorkersPane.svelte";

  interface Props {
    id: string;
    tab: ThreadTab;
    onmenu: () => void;
  }

  let { id, tab, onmenu }: Props = $props();

  let data = $state<ThreadResponse | null>(null);
  let error = $state("");
  let loading = $state(true);
  let polling = false;

  // Git state lives here rather than in ChangesPane: both the Agent and Changes
  // panes read it, and the tab strip shows a dirty dot from it. `git status`
  // shells out, so it is fetched on first use of a tab that needs it rather
  // than on every thread open — the dot appears once one of those is visited.
  let git = $state<GitResponse | null>(null);
  let gitError = $state("");
  // Thread `git` belongs to; `null` means "not fetched yet". Keying on it makes
  // the state order-independent: a result for a previous thread is filtered out
  // by `currentGit` rather than cleared by a separate reset effect.
  let gitFor = $state<string | null>(null);

  // Workers are owned here for the same reason git is: the tab strip has to
  // know whether to show the tab, and AgentPane renders a roll-up, so one
  // request serves both. `worker_count` on the thread response gates the tab
  // before this ever runs.
  let workers = $state<WorkerItem[]>([]);
  let workersError = $state("");
  let workersLoading = $state(false);
  let workersFor = $state<string | null>(null);

  // The trajectory is a shared engine projection fetched only when its pane is
  // opened. Keeping it separate avoids attaching a full-thread report to every
  // small transcript delta poll.
  let trajectory = $state<ThreadTrajectoryReport | null>(null);
  let trajectoryError = $state("");
  let trajectoryLoading = $state(false);
  let trajectoryPolling = false;
  let trajectoryFor = $state<string | null>(null);

  let currentGit = $derived(gitFor === id ? git : null);
  let currentWorkers = $derived(workersFor === id ? workers : []);
  let currentTrajectory = $derived(trajectoryFor === id ? trajectory : null);

  let workerRollup = $derived(rollupFrom(currentWorkers));
  // Fetched workers win over the count from the thread load: that count is a
  // snapshot from page load, and the whole point of discovery is that the first
  // worker can appear after it.
  let hasWorkers = $derived(currentWorkers.length > 0 || (data?.worker_count ?? 0) > 0);
  let workersBusy = $derived(isBusy(workerRollup));
  let workersAttention = $derived(needsAttention(workerRollup));

  let live = $derived(data ? data.activity.some((a) => a.type === "tool_running") : false);

  let dirty = $derived(
    currentGit
      ? currentGit.files.staged.length +
          currentGit.files.unstaged.length +
          currentGit.files.untracked.length >
        0
      : false,
  );

  async function load(showSpinner = true) {
    if (showSpinner) loading = true;
    error = "";
    try {
      data = await api.thread(id);
    } catch (e) {
      error = e instanceof ApiError ? e.message : String(e);
    } finally {
      loading = false;
    }
  }

  // A live poll asks only for activity past the last cursor and merges it in.
  // Re-fetching the whole transcript every 4s re-sent hundreds of kilobytes per
  // tick on a long thread. Running tools are always resent, so they are dropped
  // before merging and replaced by whatever the poll reports.
  async function poll() {
    if (polling || !data) return;
    polling = true;
    const target = id;
    try {
      const delta = await api.thread(target, data.cursor);
      if (target !== id || !data) return;
      if (!delta.partial) {
        data = delta;
      } else {
        const persisted = data.activity.filter((a) => a.type !== "tool_running");
        const seen = new Set(persisted.map((a) => a.sequence));
        data = {
          ...delta,
          activity: [...persisted, ...delta.activity.filter((a) => !seen.has(a.sequence))],
        };
      }
      if (tab === "trajectory") await loadTrajectory(false);
    } catch {
      // A failed poll is not worth surfacing; the next tick retries.
    } finally {
      polling = false;
    }
  }

  async function loadGit() {
    const target = id;
    gitFor = target;
    gitError = "";
    try {
      const response = await api.git(target);
      if (gitFor !== target) return; // thread changed mid-flight
      git = response;
    } catch (e) {
      if (gitFor === target) gitError = e instanceof ApiError ? e.message : String(e);
    }
  }

  async function loadWorkers(showSpinner = false) {
    const target = id;
    const first = workersFor !== target;
    workersFor = target;
    workersError = "";
    if (showSpinner || first) workersLoading = true;
    try {
      const response = await api.workers(target);
      if (workersFor !== target) return;
      workers = response.workers;
    } catch (e) {
      // A failed background refresh is not worth replacing the list with an
      // error; the next tick retries. Only the first load reports.
      if (workersFor === target && first) {
        workersError = e instanceof ApiError ? e.message : String(e);
      }
    } finally {
      if (workersFor === target) workersLoading = false;
    }
  }

  async function loadTrajectory(showSpinner = true) {
    if (trajectoryPolling) return;
    trajectoryPolling = true;
    const target = id;
    const first = trajectoryFor !== target;
    trajectoryFor = target;
    trajectoryError = "";
    if (showSpinner || first) trajectoryLoading = true;
    try {
      const response = await api.trajectory(target);
      if (trajectoryFor !== target || target !== id) return;
      trajectory = response;
    } catch (e) {
      if (trajectoryFor === target && first) {
        trajectoryError = e instanceof ApiError ? e.message : String(e);
      }
    } finally {
      if (trajectoryFor === target) trajectoryLoading = false;
      trajectoryPolling = false;
      if (target !== id && tab === "trajectory") void loadTrajectory();
    }
  }

  $effect(() => {
    void id;
    load();
  });

  // Fetch git the first time a pane that needs it is opened for this thread,
  // then keep it across tab switches.
  $effect(() => {
    if (tab !== "agent" && tab !== "changes") return;
    if (gitFor === id) return;
    loadGit();
  });

  // Fetch workers whenever a pane that reports them is open — including when
  // the thread is not known to have any. A thread becomes an orchestrator by
  // spawning its first worker, so gating the first fetch on `worker_count > 0`
  // (a page-load snapshot) means that first worker is never discovered.
  $effect(() => {
    if (tab !== "workers" && tab !== "agent") return;
    if (workersFor === id) return;
    loadWorkers();
  });

  $effect(() => {
    if (tab !== "trajectory" || trajectoryFor === id) return;
    loadTrajectory();
  });

  // Discovery and refresh run on the same timer, and neither is gated on "a
  // worker is already running": that can never observe a worker appearing, nor
  // a queued→running flip, because both happen while the client believes there
  // is nothing to watch. This reads one small endpoint — it never polls the
  // transcript or shells out to git for worker status.
  $effect(() => {
    if (tab !== "workers" && tab !== "agent") return;
    const period = workersBusy ? 4000 : 10000;
    const timer = setInterval(() => {
      if (document.visibilityState === "visible") loadWorkers();
    }, period);
    return () => clearInterval(timer);
  });

  // Poll only while a tool is actually executing, and only when visible. The
  // in-flight guard in `poll` keeps a slow response from stacking up ticks.
  $effect(() => {
    if (!live) return;
    const timer = setInterval(() => {
      if (document.visibilityState === "visible") poll();
    }, 4000);
    return () => clearInterval(timer);
  });

  // `git status` shells out, so refresh it when a turn finishes rather than on
  // every 4s transcript poll — and only if something already asked for it.
  let wasLive = false;
  $effect(() => {
    let reconcile: ReturnType<typeof setTimeout> | undefined;
    if (wasLive && !live) {
      if (gitFor === id) loadGit();
      // Tool markers clear just before checkpoint persistence lands. One
      // trailing delta closes that handoff even though the regular live timer
      // has stopped.
      reconcile = setTimeout(() => {
        if (document.visibilityState === "visible") poll();
      }, 1000);
    }
    wasLive = live;
    return () => {
      if (reconcile) clearTimeout(reconcile);
    };
  });

  function refresh() {
    haptic();
    load(false);
    if (gitFor === id) loadGit();
    if (workersFor === id) loadWorkers();
    if (trajectoryFor === id) loadTrajectory(false);
  }

  // Jump target for Artifacts "Go to message". Set to null first so tapping
  // the same row twice still re-triggers the transcript scroll effect.
  let artifactJump = $state<number | null>(null);

  function gotoMessage(sequence: number) {
    artifactJump = null;
    router.setTab("transcript");
    setTimeout(() => {
      artifactJump = sequence;
    }, 50);
  }
  /** Navigates out of a worker thread back to the orchestrator that owns it. */
  function openParent() {
    if (!data?.parent_thread_id) return;
    selectionChanged();
    router.openThread(data.parent_thread_id, "workers");
  }

  // Jumps to the Telegram topic this thread is bound to. The Mini App stays
  // open behind it (Bot API 7.0+), so this is a jump, not a close.
  function openInTelegram() {
    if (!data?.telegram_link) return;
    haptic();
    openTelegramLink(data.telegram_link);
  }

  // Tabs are data-driven so new panes (Terminal, …) are a one-line add.
  const TAB_ICONS: Record<ThreadTab, IconName> = {
    workers: "users",
    agent: "bot",
    transcript: "message-square",
    artifacts: "file",
    trajectory: "timer",
    changes: "git-branch",
  };

  // Workers leads on an orchestrator: the strip scrolls horizontally, so a tab
  // appended last starts offscreen on a phone.
  let tabs = $derived(
    ([...(hasWorkers ? (["workers"] as ThreadTab[]) : []), "trajectory", "transcript", "artifacts", "agent", "changes"] as ThreadTab[]).map(
      (t) => ({
        id: t,
        label: THREAD_TAB_LABELS[t],
        icon: TAB_ICONS[t],
        dot: (t === "changes" && dirty) || (t === "workers" && workersAttention),
      }),
    ),
  );

  // A thread with no workers has no Workers pane; a stale deep link to it would
  // otherwise render an empty tab that is not in the strip.
  let activeTab = $derived(tab === "workers" && !hasWorkers ? "transcript" : tab);
</script>

<header
  class="flex shrink-0 items-center gap-1.5 border-b border-border px-2 py-2"
  style="padding-top: calc(var(--tg-safe-top) + 0.5rem)"
>
  <button
    type="button"
    onclick={onmenu}
    aria-label="Open menu"
    class="rounded-sm p-1.5 text-muted-foreground hover:bg-accent"
  >
    <svg viewBox="0 0 24 24" class="size-4" aria-hidden="true">
      <path
        fill="none"
        stroke="currentColor"
        stroke-width="1.75"
        stroke-linecap="round"
        d="M3 6h18M3 12h18M3 18h18"
      />
    </svg>
  </button>

  <div class="min-w-0 flex-1">
    <h1 class="m-0 truncate text-[0.9375rem] leading-tight font-semibold tracking-tight">
      {data?.title ?? "Thread"}
    </h1>
    {#if data}
      <p class="m-0 truncate font-mono text-xxs text-muted-foreground">
        {data.total_messages} messages · {data.total_events} events
        {#if live}<span class="text-warning">· live</span>{/if}
      </p>
    {/if}
  </div>

  {#if data?.telegram_link}
    <button
      type="button"
      onclick={openInTelegram}
      aria-label="Open in Telegram"
      title="Open in Telegram"
      class="rounded-sm p-1.5 text-muted-foreground hover:bg-accent"
    >
      <svg viewBox="0 0 24 24" class="size-4" aria-hidden="true">
        <path
          fill="none"
          stroke="currentColor"
          stroke-width="1.75"
          stroke-linecap="round"
          stroke-linejoin="round"
          d="M21.5 3.5 2.5 10.2l6.2 2.3M21.5 3.5l-3.1 16.9-9.7-8.9M21.5 3.5 8.7 12.5m0 0v5.6l3-3.1"
        />
      </svg>
    </button>
  {/if}

  <button
    type="button"
    onclick={refresh}
    aria-label="Refresh"
    class="rounded-sm p-1.5 text-muted-foreground hover:bg-accent"
  >
    <svg viewBox="0 0 24 24" class="size-4" aria-hidden="true">
      <path
        fill="none"
        stroke="currentColor"
        stroke-width="1.75"
        stroke-linecap="round"
        stroke-linejoin="round"
        d="M21 12a9 9 0 1 1-2.64-6.36M21 3v6h-6"
      />
    </svg>
  </button>
</header>

{#if data?.parent_thread_id}
  <!-- A worker is a normal thread, so without this it is a dead end: nothing
       on screen says which orchestrator asked for the work. -->
  <div class="shrink-0 px-2.5 pt-2">
    <button
      type="button"
      onclick={openParent}
      class="flex max-w-full items-center gap-1.5 rounded-full border border-border bg-card px-2.5 py-1 text-xxs text-muted-foreground hover:bg-accent"
    >
      <svg viewBox="0 0 24 24" class="size-3 shrink-0" aria-hidden="true">
        <path
          fill="none"
          stroke="currentColor"
          stroke-width="1.75"
          stroke-linecap="round"
          stroke-linejoin="round"
          d="M15 18l-6-6 6-6"
        />
      </svg>
      <span class="truncate">{data.parent_title ?? "Orchestrator"}</span>
    </button>
  </div>
{/if}

<TabStrip {tabs} active={activeTab} onselect={(t) => router.setTab(t as ThreadTab)} />
{#if loading && activeTab !== "changes"}
  <p class="flex-1 py-8 text-center text-xs text-muted-foreground">Loading…</p>
{:else if error && activeTab !== "changes"}
  <div class="flex-1 px-3 py-3">
    <p class="rounded-md border border-destructive px-3 py-2 text-xs text-destructive">{error}</p>
  </div>
{:else if activeTab === "workers"}
  <WorkersPane
    workers={currentWorkers}
    rollup={workerRollup}
    error={workersError}
    loading={workersLoading}
  />
{:else if activeTab === "transcript"}
  <TranscriptPane activity={data?.activity ?? []} threadId={id} highlight={artifactJump} />
{:else if activeTab === "artifacts"}
  <ArtifactsPane threadId={id} ongoto={gotoMessage} />
{:else if activeTab === "trajectory"}
  {#if trajectoryLoading && !currentTrajectory}
    <p class="flex-1 py-8 text-center text-xs text-muted-foreground">Loading trajectory…</p>
  {:else if trajectoryError && !currentTrajectory}
    <div class="flex-1 px-3 py-3">
      <p class="rounded-md border border-destructive px-3 py-2 text-xs text-destructive">
        {trajectoryError}
      </p>
    </div>
  {:else if currentTrajectory}
    <TrajectoryPane trajectory={currentTrajectory} activity={data?.activity ?? []} />
  {/if}
{:else if activeTab === "agent"}
  <AgentPane
    thread={data}
    git={currentGit}
    rollup={workerRollup}
    hasWorkers={hasWorkers}
    onopenworkers={() => router.setTab("workers")}
  />
{:else}
  <ChangesPane
    {id}
    data={currentGit}
    error={gitError}
    sharedRepo={hasWorkers || !!data?.parent_thread_id}
  />
{/if}
