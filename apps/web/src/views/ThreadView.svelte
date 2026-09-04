<script lang="ts">
  import { api, ApiError } from "$lib/api";
  import type { ThreadResponse, GitResponse } from "$lib/types";
  import { router, THREAD_TAB_LABELS, type ThreadTab } from "$lib/router.svelte";
  import { haptic, openTelegramLink } from "$lib/telegram";
  import TabStrip from "../components/TabStrip.svelte";
  import type { IconName } from "../components/Icon.svelte";
  import TranscriptPane from "./TranscriptPane.svelte";
  import ChangesPane from "./ChangesPane.svelte";
  import AgentPane from "./AgentPane.svelte";

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

  let currentGit = $derived(gitFor === id ? git : null);

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
        return;
      }
      const persisted = data.activity.filter((a) => a.type !== "tool_running");
      const seen = new Set(persisted.map((a) => a.sequence));
      data = {
        ...delta,
        activity: [...persisted, ...delta.activity.filter((a) => !seen.has(a.sequence))],
      };
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
    if (wasLive && !live && gitFor === id) loadGit();
    wasLive = live;
  });

  function refresh() {
    haptic();
    load(false);
    if (gitFor === id) loadGit();
  }

  // Jumps to the Telegram topic this thread is bound to. The Mini App stays
  // open behind it (Bot API 7.0+), so this is a jump, not a close.
  function openInTelegram() {
    if (!data?.telegram_link) return;
    haptic();
    openTelegramLink(data.telegram_link);
  }

  // Tabs are data-driven so new panes (Files, Terminal, …) are a one-line add.
  const TAB_ICONS: Record<ThreadTab, IconName> = {
    agent: "bot",
    transcript: "message-square",
    changes: "git-branch",
  };

  let tabs = $derived(
    (["agent", "transcript", "changes"] as ThreadTab[]).map((t) => ({
      id: t,
      label: THREAD_TAB_LABELS[t],
      icon: TAB_ICONS[t],
      dot: t === "changes" && dirty,
    })),
  );
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

<TabStrip {tabs} active={tab} onselect={(t) => router.setTab(t as ThreadTab)} />

{#if loading && tab !== "changes"}
  <p class="flex-1 py-8 text-center text-xs text-muted-foreground">Loading…</p>
{:else if error && tab !== "changes"}
  <div class="flex-1 px-3 py-3">
    <p class="rounded-md border border-destructive px-3 py-2 text-xs text-destructive">{error}</p>
  </div>
{:else if tab === "transcript"}
  <TranscriptPane activity={data?.activity ?? []} />
{:else if tab === "agent"}
  <AgentPane thread={data} git={currentGit} />
{:else}
  <ChangesPane {id} data={currentGit} error={gitError} />
{/if}
