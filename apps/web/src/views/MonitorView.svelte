<script lang="ts">
  import { api, ApiError } from "$lib/api";
  import type { MonitorResponse } from "$lib/types";
  import { router, MONITOR_SECTION_LABELS, type MonitorSection } from "$lib/router.svelte";
  import { haptic } from "$lib/telegram";

  interface Props {
    section: MonitorSection;
    onmenu: () => void;
  }

  let { section, onmenu }: Props = $props();

  let data = $state<MonitorResponse | null>(null);
  let error = $state("");
  let loading = $state(true);

  async function load(showSpinner = true) {
    if (showSpinner) loading = true;
    error = "";
    try {
      data = await api.monitor();
    } catch (e) {
      error = e instanceof ApiError ? e.message : String(e);
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    load();
  });

  // The server caches this for 30s, so polling faster only burns battery.
  $effect(() => {
    const timer = setInterval(() => {
      if (document.visibilityState === "visible") load(false);
    }, 30_000);
    return () => clearInterval(timer);
  });

  let show = $derived((s: MonitorSection) => section === "overview" || section === s);

  function tokens(n: number): string {
    if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
    if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
    return String(n);
  }

  let peakQuota = $derived(
    Math.max(
      0,
      ...(data?.subscriptions.flatMap((s) => s.windows.map((w) => w.used_percent)) ?? [0]),
    ),
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

  <h1 class="m-0 min-w-0 flex-1 truncate text-[0.9375rem] leading-tight font-semibold tracking-tight">
    {MONITOR_SECTION_LABELS[section]}
  </h1>

  <button
    type="button"
    onclick={() => {
      haptic();
      load(false);
    }}
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

<div class="scroll-area flex-1 px-3 py-3">
  {#if loading}
    <p class="py-8 text-center text-xs text-muted-foreground">Loading…</p>
  {:else if error}
    <p class="rounded-md border border-destructive px-3 py-2 text-xs text-destructive">{error}</p>
  {:else if data}
    <div class="mx-auto flex max-w-3xl flex-col gap-3 pb-6">
      {#if section === "overview"}
        <div class="grid grid-cols-3 gap-2">
          {#each [{ label: "services", value: `${data.services.filter((s) => s.running).length}/${data.services.length}` }, { label: "agents", value: String(data.active_agents.length) }, { label: "quota", value: `${peakQuota.toFixed(0)}%` }] as m (m.label)}
            <div class="rounded-lg border border-border bg-card px-2.5 py-2">
              <p class="m-0 font-mono text-xxs uppercase text-muted-foreground">{m.label}</p>
              <p class="m-0 text-lg font-semibold tracking-tight tabular-nums">{m.value}</p>
            </div>
          {/each}
        </div>
      {/if}

      {#if show("agents")}
        <section>
          <h2 class="sec">Active agents</h2>
          {#if data.active_agents.length === 0}
            <p class="rounded-md border border-border bg-card px-2.5 py-2 text-xs text-muted-foreground">
              No agents running.
            </p>
          {:else}
            <div class="flex flex-col gap-1.5">
              {#each data.active_agents as agent, ai (ai)}
                <button
                  type="button"
                  disabled={!agent.thread_id}
                  onclick={() => agent.thread_id && router.openThread(agent.thread_id, "agent")}
                  class="w-full rounded-md border border-border bg-card px-2.5 py-2 text-left disabled:opacity-60"
                >
                  <div class="flex items-baseline gap-2">
                    <span class="font-mono text-xs font-medium"
                      >{agent.role ?? agent.surface ?? "agent"}</span
                    >
                    <span class="ml-auto font-mono text-xxs text-muted-foreground"
                      >{agent.uptime}</span
                    >
                  </div>
                  <p class="m-0 truncate font-mono text-xxs text-muted-foreground">
                    {agent.model ?? "—"}{agent.current_tool
                      ? ` · ${agent.current_tool}`
                      : agent.phase
                        ? ` · ${agent.phase}`
                        : ""}
                  </p>
                </button>
              {/each}
            </div>
          {/if}
        </section>
      {/if}

      {#if show("services")}
        <section>
          <h2 class="sec">Services</h2>
          <div class="flex flex-col gap-1.5">
            {#each data.services as svc (svc.name)}
              <div
                class="flex items-center gap-2 rounded-md border border-border bg-card px-2.5 py-1.5"
              >
                <span
                  class="size-1.5 rounded-full {svc.running
                    ? 'bg-success'
                    : 'bg-muted-foreground/40'}"
                ></span>
                <span class="font-mono text-xs">{svc.name}</span>
                <span class="ml-auto font-mono text-xxs text-muted-foreground">
                  {svc.running ? (svc.uptime ?? "up") : "stopped"}
                </span>
              </div>
            {/each}
          </div>
        </section>

        {#if data.background_processes.length}
          <section>
            <h2 class="sec">Background</h2>
            <div class="flex flex-col gap-1.5">
              {#each data.background_processes as bg (bg.id)}
                <div class="rounded-md border border-border bg-card px-2.5 py-1.5">
                  <p class="m-0 truncate font-mono text-xs">{bg.command}</p>
                  <p class="m-0 font-mono text-xxs text-muted-foreground">
                    {bg.id} · pid {bg.pid} · {bg.uptime}
                  </p>
                </div>
              {/each}
            </div>
          </section>
        {/if}
      {/if}

      {#if show("usage")}
        {#if data.subscriptions.length}
          <section>
            <h2 class="sec">Subscriptions</h2>
            <div class="flex flex-col gap-1.5">
              {#each data.subscriptions as sub, si (si)}
                <div class="rounded-md border border-border bg-card px-2.5 py-2">
                  <div class="flex items-baseline gap-2">
                    <span class="font-mono text-xs font-medium">{sub.name}</span>
                    {#if sub.plan}
                      <span class="font-mono text-xxs text-muted-foreground">{sub.plan}</span>
                    {/if}
                  </div>
                  {#if sub.error}
                    <p class="m-0 mt-1 text-xxs text-destructive">{sub.error}</p>
                  {/if}
                  <!-- Keyed by index: a provider can report several windows with
                       the same label (two `weekly` quotas on different scopes),
                       so the label is not unique and would abort the render. -->
                  {#each sub.windows as w, wi (wi)}
                    <div class="mt-1.5">
                      <div class="flex items-baseline justify-between font-mono text-xxs">
                        <span class="text-muted-foreground">{w.label}</span>
                        <span class="tabular-nums">{w.used_percent.toFixed(0)}%</span>
                      </div>
                      <div class="mt-0.5 h-1 overflow-hidden rounded-full bg-muted">
                        <div
                          class="h-full rounded-full"
                          class:bg-success={w.used_percent < 70}
                          class:bg-warning={w.used_percent >= 70 && w.used_percent < 90}
                          class:bg-destructive={w.used_percent >= 90}
                          style="width: {w.used_percent}%"
                        ></div>
                      </div>
                    </div>
                  {/each}
                </div>
              {/each}
            </div>
          </section>
        {/if}

        {#if data.usage}
          <section>
            <h2 class="sec">Usage · {data.usage.span}</h2>
            <div class="rounded-md border border-border bg-card px-2.5 py-2">
              <div class="flex items-baseline gap-3 font-mono text-xxs">
                <span>{tokens(data.usage.tokens)} tokens</span>
                <span class="text-muted-foreground">{data.usage.requests} req</span>
                <span class="ml-auto">${data.usage.billed_usd.toFixed(2)}</span>
              </div>
              {#if data.usage.daily.length}
                {@const peak = Math.max(...data.usage.daily.map((d) => d.tokens), 1)}
                <div class="mt-2 flex h-10 items-end gap-px">
                  {#each data.usage.daily as d (d.day)}
                    <div
                      class="flex-1 rounded-t-xs bg-foreground/25"
                      style="height: {Math.max(2, (d.tokens / peak) * 100)}%"
                      title="{d.tokens.toLocaleString()} tokens"
                    ></div>
                  {/each}
                </div>
              {/if}
            </div>
          </section>
        {/if}
      {/if}

      {#if show("config")}
        <section>
          <h2 class="sec">Config</h2>
          <dl class="rounded-md border border-border bg-card px-2.5 py-2 font-mono text-xxs">
            {#each [["model", data.config.model], ["thinking", data.config.thinking], ["max tokens", data.config.max_tokens === null ? "—" : String(data.config.max_tokens)], ["tool timeout", `${data.config.tool_timeout_secs}s`], ["subagents", data.config.subagents_enabled ? "on" : "off"], ["modes", String(data.config.mode_count)], ["server port", String(data.config.server_port)]] as [k, v] (k)}
              <div class="flex justify-between gap-3 py-0.5">
                <dt class="text-muted-foreground">{k}</dt>
                <dd class="m-0 truncate text-right">{v}</dd>
              </div>
            {/each}
          </dl>
        </section>

        {#if data.config.helper_models.length}
          <section>
            <h2 class="sec">Helper models</h2>
            <dl class="rounded-md border border-border bg-card px-2.5 py-2 font-mono text-xxs">
              {#each data.config.helper_models as h (h.role)}
                <div class="flex justify-between gap-3 py-0.5">
                  <dt class="text-muted-foreground">{h.role}</dt>
                  <dd class="m-0 truncate text-right">{h.model}</dd>
                </div>
              {/each}
            </dl>
          </section>
        {/if}
      {/if}

      {#if show("automations")}
        <section>
          <h2 class="sec">Automations</h2>
          {#if data.automations.length === 0}
            <p class="rounded-md border border-border bg-card px-2.5 py-2 text-xs text-muted-foreground">
              None configured.
            </p>
          {:else}
            <div class="flex flex-col gap-1">
              {#each data.automations as a (a.name)}
                <div
                  class="flex items-center gap-2 rounded-md border border-border bg-card px-2.5 py-1.5"
                >
                  <span class="truncate font-mono text-xs">{a.name}</span>
                  <span class="ml-auto font-mono text-xxs text-muted-foreground">
                    {a.schedule ?? "manual"}
                  </span>
                </div>
              {/each}
            </div>
          {/if}
        </section>
      {/if}
    </div>
  {/if}
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
