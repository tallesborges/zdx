<script lang="ts">
  import { api, ApiError } from "$lib/api";
  import type { GitResponse, MonitorResponse, ThreadResponse, UsageActivity } from "$lib/types";

  interface Props {
    thread: ThreadResponse | null;
    /** Fetched by ThreadView; supplies the working folder and branch. */
    git: GitResponse | null;
  }

  let { thread, git }: Props = $props();

  let monitor = $state<MonitorResponse | null>(null);
  let error = $state("");

  async function load() {
    error = "";
    try {
      monitor = await api.monitor();
    } catch (e) {
      error = e instanceof ApiError ? e.message : String(e);
    }
  }

  $effect(() => {
    load();
  });

  /** The live agent process driving this thread, if one is running. */
  let agent = $derived(
    thread && monitor
      ? (monitor.active_agents.find((a) => a.thread_id === thread.id) ?? null)
      : null,
  );

  /** Most recent usage event, for model/provider attribution. */
  let lastUsage = $derived.by(() => {
    if (!thread) return null;
    for (let i = thread.activity.length - 1; i >= 0; i--) {
      const item = thread.activity[i];
      if (item.type === "usage") return item as UsageActivity;
    }
    return null;
  });

  // Providers emit two kinds of usage event: one carrying the prompt (input +
  // cache) and one carrying only the completion. Only the former describes what
  // occupies the window, so the plain "last event" is usually ~0 and useless.
  let context = $derived.by(() => {
    if (!thread) return null;
    for (let i = thread.activity.length - 1; i >= 0; i--) {
      const item = thread.activity[i];
      if (item.type !== "usage") continue;
      const u = item as UsageActivity;
      const prompt = u.input_tokens + u.cache_read_tokens + u.cache_write_tokens;
      if (prompt === 0) continue;
      const used = prompt + u.output_tokens;
      const limit = u.context_limit ?? null;
      return { used, limit, pct: limit ? (used / limit) * 100 : null };
    }
    return null;
  });

  // Roll every usage event into a per-thread total.
  let totals = $derived.by(() => {
    const acc = { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, turns: 0, ttft: [] as number[] };
    if (!thread) return acc;
    for (const item of thread.activity) {
      if (item.type !== "usage") continue;
      const u = item as UsageActivity;
      acc.input += u.input_tokens;
      acc.output += u.output_tokens;
      acc.cacheRead += u.cache_read_tokens;
      acc.cacheWrite += u.cache_write_tokens;
      acc.turns += 1;
      if (u.ttft_ms !== undefined) acc.ttft.push(u.ttft_ms);
    }
    return acc;
  });

  /** Share of prompt tokens served from cache — the main cost lever. */
  let cacheHit = $derived.by(() => {
    const prompt = totals.input + totals.cacheRead;
    return prompt > 0 ? (totals.cacheRead / prompt) * 100 : null;
  });

  let medianTtft = $derived.by(() => {
    const v = [...totals.ttft].sort((a, b) => a - b);
    return v.length ? v[Math.floor(v.length / 2)] : null;
  });

  function fmt(n: number): string {
    if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(2)}M`;
    if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
    return String(n);
  }

  function limitLabel(n: number): string {
    return n >= 1_000_000 ? `${(n / 1_000_000).toFixed(1)}M` : `${Math.round(n / 1000)}k`;
  }

  let model = $derived(agent?.model ?? lastUsage?.model ?? monitor?.config.model ?? "—");
  let provider = $derived(agent?.provider ?? lastUsage?.provider ?? "—");
  let thinking = $derived(agent?.thinking ?? monitor?.config.thinking ?? "—");
  let folder = $derived(git?.repository.root ?? null);
</script>

<div class="scroll-area flex-1 px-3 py-3">
  {#if error}
    <p class="rounded-md border border-destructive px-3 py-2 text-xs text-destructive">{error}</p>
  {/if}

  <div class="mx-auto flex max-w-3xl flex-col gap-3 pb-6">
    <!-- Status -->
    <div class="rounded-lg border border-border bg-card px-3 py-2.5">
      <div class="flex items-center gap-2">
        <span
          class="size-1.5 rounded-full {agent ? 'bg-warning' : 'bg-muted-foreground/40'}"
          class:animate-pulse={!!agent}
        ></span>
        <span class="text-sm font-semibold tracking-tight">{agent ? "Running" : "Idle"}</span>
        {#if agent}
          <span class="ml-auto font-mono text-xxs text-muted-foreground">{agent.uptime}</span>
        {/if}
      </div>
      {#if agent}
        <p class="m-0 mt-1 truncate font-mono text-xxs text-muted-foreground">
          {agent.current_tool ?? agent.phase ?? "working"} · pid {agent.pid}
        </p>
      {/if}
    </div>

    <!-- Context occupancy -->
    {#if context}
      <section>
        <h2 class="sec">Context</h2>
        <div class="rounded-md border border-border bg-card px-2.5 py-2">
          <div class="flex items-baseline gap-2 font-mono text-xs">
            {#if context.pct !== null}
              <span class="text-base font-semibold tabular-nums">{context.pct.toFixed(0)}%</span>
              <span class="text-xxs text-muted-foreground">
                {fmt(context.used)} of {limitLabel(context.limit!)}
              </span>
            {:else}
              <span class="text-base font-semibold tabular-nums">{fmt(context.used)}</span>
              <span class="text-xxs text-muted-foreground">limit unknown</span>
            {/if}
          </div>
          {#if context.pct !== null}
            <div class="mt-1.5 h-1 overflow-hidden rounded-full bg-muted">
              <div
                class="h-full rounded-full"
                class:bg-success={context.pct < 60}
                class:bg-warning={context.pct >= 60 && context.pct < 85}
                class:bg-destructive={context.pct >= 85}
                style="width: {Math.min(100, context.pct)}%"
              ></div>
            </div>
          {/if}
        </div>
      </section>
    {/if}

    <!-- Workspace -->
    <section>
      <h2 class="sec">Workspace</h2>
      <dl class="rounded-md border border-border bg-card px-2.5 py-2 font-mono text-xxs">
        {#if folder}
          <div class="py-0.5">
            <dt class="text-muted-foreground">folder</dt>
            <dd class="m-0 mt-0.5 truncate text-right" dir="rtl">{folder}</dd>
          </div>
        {/if}
        {#each [["branch", git?.repository.branch ?? "—"], ["head", git?.repository.head ?? "—"], ["upstream", git?.repository.upstream ?? "—"], ["state", git ? (git.repository.clean ? "clean" : "changed") : "—"]] as [k, v] (k)}
          <div class="flex justify-between gap-3 py-0.5">
            <dt class="text-muted-foreground">{k}</dt>
            <dd class="m-0 truncate text-right">{v}</dd>
          </div>
        {/each}
      </dl>
    </section>

    <!-- Model -->
    <section>
      <h2 class="sec">Model</h2>
      <dl class="rounded-md border border-border bg-card px-2.5 py-2 font-mono text-xxs">
        {#each [["model", model], ["provider", provider], ["thinking", thinking], ["account", agent?.account ?? "—"], ["surface", agent?.surface ?? "telegram"], ["role", agent?.role ?? "—"]] as [k, v] (k)}
          <div class="flex justify-between gap-3 py-0.5">
            <dt class="text-muted-foreground">{k}</dt>
            <dd class="m-0 truncate text-right">{v}</dd>
          </div>
        {/each}
      </dl>
    </section>

    <!-- Usage -->
    <section>
      <h2 class="sec">Usage · {totals.turns} turns</h2>
      <div class="rounded-md border border-border bg-card px-2.5 py-2">
        <div class="grid grid-cols-2 gap-x-4 font-mono text-xxs">
          {#each [["↑ input", fmt(totals.input)], ["↓ output", fmt(totals.output)], ["R cache", fmt(totals.cacheRead)], ["W cache", fmt(totals.cacheWrite)]] as [k, v] (k)}
            <div class="flex justify-between py-0.5">
              <span class="text-muted-foreground">{k}</span>
              <span class="tabular-nums">{v}</span>
            </div>
          {/each}
        </div>
        {#if cacheHit !== null || medianTtft !== null}
          <div
            class="mt-1 flex gap-4 border-t border-border pt-1 font-mono text-xxs text-muted-foreground"
          >
            {#if cacheHit !== null}
              <span>cache hit <span class="text-success">{cacheHit.toFixed(0)}%</span></span>
            {/if}
            {#if medianTtft !== null}
              <span class="ml-auto">median ttft {medianTtft}ms</span>
            {/if}
          </div>
        {/if}
      </div>
    </section>

    <!-- Thread -->
    {#if thread}
      <section>
        <h2 class="sec">Thread</h2>
        <dl class="rounded-md border border-border bg-card px-2.5 py-2 font-mono text-xxs">
          {#each [["messages", String(thread.total_messages)], ["events", String(thread.total_events)], ["parent", agent?.parent_thread_id ?? "—"]] as [k, v] (k)}
            <div class="flex justify-between gap-3 py-0.5">
              <dt class="text-muted-foreground">{k}</dt>
              <dd class="m-0 truncate text-right">{v}</dd>
            </div>
          {/each}
          <div class="py-0.5">
            <dt class="text-muted-foreground">id</dt>
            <dd class="m-0 mt-0.5 truncate text-right" dir="rtl">{thread.id}</dd>
          </div>
        </dl>
      </section>
    {/if}
  </div>
</div>

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
</style>
