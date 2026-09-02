<script lang="ts">
  import { api, ApiError } from "$lib/api";
  import type { ThreadListItem } from "$lib/types";
  import { router } from "$lib/router.svelte";
  import { haptic, openTelegramLink, selectionChanged } from "$lib/telegram";

  interface Props {
    onmenu: () => void;
  }

  let { onmenu }: Props = $props();

  let threads = $state<ThreadListItem[]>([]);
  let error = $state("");
  let loading = $state(true);

  async function load(showSpinner = true) {
    if (showSpinner) loading = true;
    error = "";
    try {
      threads = (await api.threads()).threads;
    } catch (e) {
      error = e instanceof ApiError ? e.message : String(e);
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    load();
  });

  function open(thread: ThreadListItem) {
    selectionChanged();
    router.openThread(thread.id);
  }

  // Jumping to Telegram leaves the Mini App open behind it (Bot API 7.0+), so
  // this is a jump the user can come back from, not a close.
  function jump(event: MouseEvent, link: string) {
    event.stopPropagation();
    haptic();
    openTelegramLink(link);
  }
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
    Recent threads
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
  {:else if threads.length === 0}
    <p class="rounded-md border border-border bg-card px-2.5 py-2 text-xs text-muted-foreground">
      No saved threads yet.
    </p>
  {:else}
    <div class="mx-auto flex max-w-3xl flex-col gap-1.5 pb-6">
      {#each threads as thread (thread.id)}
        <div class="flex items-stretch gap-1.5">
          <button
            type="button"
            onclick={() => open(thread)}
            class="min-w-0 flex-1 rounded-md border border-border bg-card px-2.5 py-2 text-left hover:bg-accent"
          >
            <div class="flex items-baseline gap-2">
              <span class="min-w-0 flex-1 truncate text-xs font-medium">{thread.title}</span>
              {#if thread.age}
                <span class="shrink-0 font-mono text-xxs text-muted-foreground">{thread.age}</span>
              {/if}
            </div>
            <p class="m-0 truncate font-mono text-xxs text-muted-foreground">
              {thread.project ?? thread.root_path ?? "—"}
            </p>
          </button>

          {#if thread.telegram_link}
            <button
              type="button"
              onclick={(e) => jump(e, thread.telegram_link!)}
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
