<script lang="ts">
  import { untrack } from "svelte";
  import { api, ApiError } from "$lib/api";
  import type { RunUsage, UsageCounts, UsageResponse } from "$lib/types";

  interface Props {
    /** Bumped by the Monitor refresh button. */
    refresh: number;
  }

  let { refresh }: Props = $props();

  const WINDOWS = [7, 30] as const;
  const COLLAPSED_ROWS = 6;

  let days = $state<number>(WINDOWS[0]);
  let data = $state<UsageResponse | null>(null);
  let error = $state("");
  let loading = $state(true);
  let showAllProjects = $state(false);
  let showAllRuns = $state(false);

  $effect(() => {
    const window = days;
    void refresh;
    untrack(() => load(window));
  });

  async function load(window: number) {
    loading = data === null || data.days !== window;
    error = "";
    try {
      const next = await api.usage(window);
      // A slower answer for the previous window must not replace this one.
      if (window === days) data = next;
    } catch (e) {
      if (window === days) error = e instanceof ApiError ? e.message : String(e);
    } finally {
      if (window === days) loading = false;
    }
  }

  function tokens(n: number): string {
    if (n >= 1_000_000_000) return `${(n / 1_000_000_000).toFixed(1)}B`;
    if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
    if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
    return String(n);
  }

  function share(part: number, total: number): string {
    if (total <= 0) return "—";
    const pct = (part / total) * 100;
    if (pct > 0 && pct < 1) return "<1%";
    return `${Math.round(pct)}%`;
  }

  function width(part: number, total: number): string {
    return `${total > 0 ? Math.max(0.75, (part / total) * 100) : 0}%`;
  }

  // Display only: the home directory reads as `~`.
  function homeless(path: string): string {
    return path.replace(/^\/(Users|home)\/[^/]+(?=\/|$)/, "~");
  }

  function baseName(path: string): string {
    const parts = homeless(path).split("/").filter(Boolean);
    return parts.at(-1) ?? path;
  }

  function parentPath(path: string): string {
    const short = homeless(path);
    const cut = short.lastIndexOf("/");
    return cut > 0 ? short.slice(0, cut) : "";
  }

  const KIND_LABELS: Record<RunUsage["kind"], string> = {
    agent: "agent",
    profile: "profile",
    worker: "worker",
    subagent: "subagent",
    helper: "helper",
    other: "other",
    unknown: "unknown",
  };

  function runTitle(row: RunUsage): string {
    if (row.name) return row.name;
    if (row.kind === "agent") return "Direct threads";
    if (row.kind === "worker") return "Workers";
    if (row.kind === "unknown") return "Not in thread index";
    return KIND_LABELS[row.kind];
  }

  function detail(row: UsageCounts): string {
    const threads = `${row.threads.toLocaleString()} ${row.threads === 1 ? "thread" : "threads"}`;
    return `${threads} · ${row.requests.toLocaleString()} req · ${tokens(row.output)} out`;
  }

  // Token classes, largest first in practice: cache reads dominate long
  // agent threads, which is the first thing to know when reading the total.
  let mix = $derived(
    data
      ? [
          { label: "cache read", value: data.cache_read, tone: "bg-foreground/25" },
          { label: "cache write", value: data.cache_write, tone: "bg-warning/70" },
          { label: "input", value: data.input, tone: "bg-link/70" },
          { label: "output", value: data.output, tone: "bg-success/80" },
        ]
      : [],
  );

  let projects = $derived(
    data ? (showAllProjects ? data.by_project : data.by_project.slice(0, COLLAPSED_ROWS)) : [],
  );
  let runs = $derived(data ? (showAllRuns ? data.by_run : data.by_run.slice(0, COLLAPSED_ROWS)) : []);
</script>

<section aria-labelledby="usage-heading">
  <div class="mb-1.5 flex items-center gap-2">
    <h2 id="usage-heading" class="sec m-0 flex-1">Recorded usage</h2>
    <div class="flex gap-1" role="group" aria-label="Usage window">
      {#each WINDOWS as w (w)}
        <button
          type="button"
          class="chip"
          class:chip-active={days === w}
          aria-pressed={days === w}
          onclick={() => {
            days = w;
            showAllProjects = false;
            showAllRuns = false;
          }}
        >
          {w}d
        </button>
      {/each}
    </div>
  </div>

  {#if loading && !data}
    <p class="rounded-md border border-border bg-card px-2.5 py-2 text-xs text-muted-foreground">
      Loading usage…
    </p>
  {:else if error}
    <p class="rounded-md border border-destructive px-2.5 py-2 text-xs text-destructive">{error}</p>
  {:else if data}
    <div class="flex flex-col gap-3" class:opacity-60={loading}>
      <div class="rounded-md border border-border bg-card px-2.5 py-2">
        <div class="flex items-baseline gap-2">
          <span class="text-lg font-semibold tracking-tight tabular-nums">{tokens(data.tokens)}</span>
          <span class="font-mono text-xxs text-muted-foreground">tokens · last {data.days} days</span>
          <span class="ml-auto font-mono text-xxs tabular-nums">
            {data.billed_usd > 0 ? `$${data.billed_usd.toFixed(2)} billed` : ""}
          </span>
        </div>
        <p class="m-0 font-mono text-xxs text-muted-foreground tabular-nums">
          {data.requests.toLocaleString()} requests
        </p>

        {#if data.tokens > 0}
          <div class="mt-2 flex h-1.5 overflow-hidden rounded-full bg-muted" aria-hidden="true">
            {#each mix as m (m.label)}
              {#if m.value > 0}
                <div class="h-full {m.tone}" style="width: {(m.value / data.tokens) * 100}%"></div>
              {/if}
            {/each}
          </div>
          <ul class="m-0 mt-1 flex list-none flex-wrap gap-x-3 gap-y-0.5 p-0 font-mono text-xxs text-muted-foreground">
            {#each mix as m (m.label)}
              <li class="flex items-center gap-1">
                <span class="inline-block size-1.5 rounded-full {m.tone}"></span>
                {m.label}
                <span class="tabular-nums text-foreground">{share(m.value, data.tokens)}</span>
              </li>
            {/each}
          </ul>
        {/if}

        {#if data.daily.length}
          {@const peak = Math.max(...data.daily.map((d) => d.tokens), 1)}
          <div class="mt-2 flex h-10 items-end gap-px">
            {#each data.daily as d, di (di)}
              <div
                class="flex-1 rounded-t-xs bg-foreground/25"
                style="height: {Math.max(2, (d.tokens / peak) * 100)}%"
                title="{d.tokens.toLocaleString()} tokens"
              ></div>
            {/each}
          </div>
        {/if}

        <p class="m-0 mt-2 text-xxs leading-snug text-muted-foreground">
          What ZDX recorded from provider responses, not your provider quota. Quota windows are
          metered by the provider and include use outside ZDX.
        </p>
      </div>

      <div>
        <h3 class="sec">By project</h3>
        {#if data.by_project.length === 0}
          <p class="rounded-md border border-border bg-card px-2.5 py-2 text-xs text-muted-foreground">
            No usage in this window.
          </p>
        {:else}
          <ol class="m-0 flex list-none flex-col divide-y divide-border rounded-md border border-border bg-card p-0">
            {#each projects as row, pi (pi)}
              <li class="px-2.5 py-1.5">
                <div class="flex items-baseline gap-2">
                  {#if row.project}
                    <span class="flex min-w-0 flex-1 items-baseline gap-1.5" title={row.project}>
                      <span class="min-w-0 truncate font-mono text-xs font-medium">
                        {baseName(row.project)}
                      </span>
                      <span class="min-w-0 shrink-[4] truncate font-mono text-xxs text-muted-foreground">
                        {parentPath(row.project)}
                      </span>
                    </span>
                  {:else}
                    <span class="flex-1 font-mono text-xs text-muted-foreground italic">Unknown project</span>
                  {/if}
                  <span class="ml-auto shrink-0 font-mono text-xxs tabular-nums text-muted-foreground">
                    {tokens(row.tokens)}
                  </span>
                  <span class="w-9 shrink-0 text-right font-mono text-xs tabular-nums">
                    {share(row.tokens, data.tokens)}
                  </span>
                </div>
                <div class="mt-1 h-1 overflow-hidden rounded-full bg-muted">
                  <div
                    class="h-full rounded-full bg-foreground-35"
                    style="width: {width(row.tokens, data.tokens)}"
                  ></div>
                </div>
                <p class="m-0 mt-0.5 truncate font-mono text-xxs text-muted-foreground">{detail(row)}</p>
              </li>
            {/each}
          </ol>
          {#if data.by_project.length > COLLAPSED_ROWS}
            <button type="button" class="more" onclick={() => (showAllProjects = !showAllProjects)}>
              {showAllProjects ? "Show fewer" : `Show all ${data.by_project.length}`}
            </button>
          {/if}
          <p class="m-0 mt-1 text-xxs leading-snug text-muted-foreground">
            A project is the thread's working folder. Subagent and helper runs count toward the thread
            that started them.
          </p>
        {/if}
      </div>

      {#if data.by_run.length}
        <div>
          <h3 class="sec">By profile, subagent, and helper</h3>
          <ol class="m-0 flex list-none flex-col divide-y divide-border rounded-md border border-border bg-card p-0">
            {#each runs as row, ri (ri)}
              <li class="px-2.5 py-1.5">
                <div class="flex items-baseline gap-2">
                  <span
                    class="min-w-0 truncate font-mono text-xs font-medium"
                    class:text-muted-foreground={row.kind === "unknown"}
                  >
                    {runTitle(row)}
                  </span>
                  <span class="kind shrink-0">{KIND_LABELS[row.kind]}</span>
                  <span class="ml-auto shrink-0 font-mono text-xxs tabular-nums text-muted-foreground">
                    {tokens(row.tokens)}
                  </span>
                  <span class="w-9 shrink-0 text-right font-mono text-xs tabular-nums">
                    {share(row.tokens, data.tokens)}
                  </span>
                </div>
                <div class="mt-1 h-1 overflow-hidden rounded-full bg-muted">
                  <div
                    class="h-full rounded-full bg-foreground-35"
                    style="width: {width(row.tokens, data.tokens)}"
                  ></div>
                </div>
                <p class="m-0 mt-0.5 truncate font-mono text-xxs text-muted-foreground">{detail(row)}</p>
              </li>
            {/each}
          </ol>
          {#if data.by_run.length > COLLAPSED_ROWS}
            <button type="button" class="more" onclick={() => (showAllRuns = !showAllRuns)}>
              {showAllRuns ? "Show fewer" : `Show all ${data.by_run.length}`}
            </button>
          {/if}
          <p class="m-0 mt-1 text-xxs leading-snug text-muted-foreground">
            Each run is counted once, in its own row. A subagent's tokens are not part of the thread
            that called it.
            {#if data.unindexed_threads > 0}
              {data.unindexed_threads} threads are not in the thread index, so their project and kind
              are unknown.
            {/if}
          </p>
        </div>
      {/if}
    </div>
  {/if}
</section>

<style>
  .sec {
    margin: 0 0 0.4rem;
    font-family: var(--font-mono);
    font-size: 0.625rem;
    font-weight: 500;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--color-muted-foreground);
  }

  .chip {
    padding: 0.0625rem 0.5rem;
    border-radius: 999px;
    border: 1px solid var(--color-border);
    background: var(--color-card);
    color: var(--color-muted-foreground);
    font-family: var(--font-mono);
    font-size: 0.6875rem;
  }

  .chip-active {
    border-color: color-mix(in srgb, var(--color-brand) 40%, transparent);
    background: color-mix(in srgb, var(--color-brand) 8%, transparent);
    color: var(--color-brand);
  }

  .kind {
    padding: 0 0.3rem;
    border: 1px solid var(--color-border);
    border-radius: 0.25rem;
    font-family: var(--font-mono);
    font-size: 0.5625rem;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--color-muted-foreground);
  }

  .bg-foreground-35 {
    background: color-mix(in srgb, var(--color-foreground) 35%, transparent);
  }

  .more {
    margin-top: 0.25rem;
    font-family: var(--font-mono);
    font-size: 0.625rem;
    color: var(--color-link);
  }
</style>
