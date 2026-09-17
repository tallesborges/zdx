<script lang="ts">
  import type { WorkerItem, WorkerRollup, WorkerStatus } from "$lib/types";
  import { router } from "$lib/router.svelte";
  import { haptic, openTelegramLink, selectionChanged } from "$lib/telegram";
  import { formatRollup } from "$lib/workers";

  interface Props {
    workers: WorkerItem[];
    rollup: WorkerRollup;
    error: string;
    loading: boolean;
  }

  let { workers, rollup, error, loading }: Props = $props();

  let summary = $derived(formatRollup(rollup));

  // Mirrors the Telegram status card's glyph semantics, mapped onto the
  // design tokens rather than emoji.
  const DOT: Record<WorkerStatus, string> = {
    running: "bg-warning",
    queued: "bg-muted-foreground/50",
    completed: "bg-success",
    failed: "bg-destructive",
    cancelled: "bg-muted-foreground/30",
    unknown: "bg-muted-foreground/30",
  };

  function elapsed(seconds: number): string {
    if (seconds < 60) return `${seconds}s`;
    const m = Math.floor(seconds / 60);
    if (m < 60) return `${m}m${String(seconds % 60).padStart(2, "0")}s`;
    return `${Math.floor(m / 60)}h${String(m % 60).padStart(2, "0")}m`;
  }

  /** Right-hand status column: elapsed while running, else the state word. */
  function trailing(worker: WorkerItem): string {
    if (!worker.live) return worker.age ?? "unknown";
    if (worker.status === "running" && worker.turn_elapsed_seconds !== null) {
      return elapsed(worker.turn_elapsed_seconds);
    }
    return worker.status;
  }

  function open(worker: WorkerItem) {
    selectionChanged();
    router.openThread(worker.thread_id);
  }

  function jump(event: MouseEvent, link: string) {
    event.stopPropagation();
    haptic();
    openTelegramLink(link);
  }
</script>

<div class="scroll-area flex-1 px-3 py-3">
  {#if error}
    <p class="rounded-md border border-destructive px-3 py-2 text-xs text-destructive">{error}</p>
  {:else if loading && workers.length === 0}
    <p class="py-8 text-center text-xs text-muted-foreground">Loading…</p>
  {:else if workers.length === 0}
    <p class="rounded-md border border-border bg-card px-2.5 py-2 text-xs text-muted-foreground">
      No workers yet.
    </p>
  {:else}
    <div class="mx-auto flex max-w-3xl flex-col gap-1.5 pb-6">
      <p class="sec">{summary}</p>

      {#each workers as worker (worker.thread_id)}
        <div class="flex items-stretch gap-1.5">
          <button
            type="button"
            onclick={() => open(worker)}
            class="min-w-0 flex-1 rounded-md border bg-card px-2.5 py-2 text-left hover:bg-accent"
            class:border-border={worker.live}
            class:border-dashed={!worker.live}
            class:border-muted-foreground={!worker.live}
          >
            <div class="flex items-baseline gap-2">
              <span class="size-1.5 shrink-0 rounded-full {DOT[worker.status]}" class:animate-pulse={worker.status === "running"}></span>
              <span
                class="min-w-0 flex-1 truncate text-xs font-medium"
                class:text-muted-foreground={!worker.live}>{worker.title}</span
              >
              <span class="shrink-0 font-mono text-xxs text-muted-foreground">
                {trailing(worker)}
              </span>
            </div>

            {#if !worker.live}
              <!-- Persisted lineage restores discovery, never runtime state. -->
              <p class="m-0 font-mono text-xxs text-muted-foreground">
                status unavailable — restarted since this ran
              </p>
            {:else}
              {#if worker.current_tool}
                <p class="m-0 truncate font-mono text-xxs text-muted-foreground">
                  ▸ {worker.current_tool}{worker.current_tool_input
                    ? ` · ${worker.current_tool_input}`
                    : ""}
                </p>
              {/if}
              {#if worker.queue_depth > 0}
                <p class="m-0 font-mono text-xxs text-muted-foreground">
                  {worker.queue_depth} prompt{worker.queue_depth === 1 ? "" : "s"} waiting
                </p>
              {/if}
              {#if worker.last_error}
                <p class="m-0 truncate font-mono text-xxs text-destructive">{worker.last_error}</p>
              {/if}
              {#if worker.project || worker.seconds_since_last_activity !== null}
                <p class="m-0 truncate font-mono text-xxs text-muted-foreground opacity-70">
                  {worker.project ?? ""}{worker.project &&
                  worker.seconds_since_last_activity !== null
                    ? " · "
                    : ""}{worker.seconds_since_last_activity !== null
                    ? `idle ${elapsed(worker.seconds_since_last_activity)}`
                    : ""}
                </p>
              {/if}
            {/if}
          </button>

          {#if worker.telegram_link}
            <button
              type="button"
              onclick={(e) => jump(e, worker.telegram_link!)}
              aria-label="Open in Telegram"
              title="Open in Telegram"
              class="shrink-0 rounded-md border border-border bg-card px-2.5 text-muted-foreground hover:bg-accent"
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
        </div>
      {/each}
    </div>
  {/if}
</div>

<style>
  .sec {
    margin: 0 0 0.1rem;
    font-family: var(--font-mono);
    font-size: 0.625rem;
    font-weight: 500;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--color-muted-foreground);
  }
</style>
