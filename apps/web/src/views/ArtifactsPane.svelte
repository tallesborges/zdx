<script lang="ts">
  import { untrack } from "svelte";
  import { api, formatBytes, ApiError } from "$lib/api";
  import type { ArtifactItem } from "$lib/types";
  import ArtifactPreview from "../components/ArtifactPreview.svelte";
  import Icon from "../components/Icon.svelte";

  interface Props {
    threadId: string;
    ongoto: (sequence: number) => void;
  }

  let { threadId, ongoto }: Props = $props();

  let items = $state<ArtifactItem[]>([]);
  let error = $state("");
  let loading = $state(true);
  let preview = $state<ArtifactItem | null>(null);
  /** Thread the current `items` belong to; also the in-flight staleness key. */
  let loadedFor = $state<string | null>(null);

  function sourceLabel(source: ArtifactItem["source"]): string {
    return source === "both" ? "generated + sent" : source;
  }

  async function load() {
    const target = threadId;
    loadedFor = target;
    loading = true;
    error = "";
    try {
      const response = await api.artifacts(target);
      if (loadedFor !== target) return;
      items = response.artifacts;
    } catch (e) {
      if (loadedFor === target) error = e instanceof ApiError ? e.message : String(e);
    } finally {
      if (loadedFor === target) loading = false;
    }
  }

  async function download(item: ArtifactItem) {
    const blob = await api.artifactBlob(threadId, item.path);
    const objectUrl = URL.createObjectURL(blob);
    const anchor = document.createElement("a");
    anchor.href = objectUrl;
    anchor.download = item.name;
    document.body.appendChild(anchor);
    anchor.click();
    anchor.remove();
    setTimeout(() => URL.revokeObjectURL(objectUrl), 30_000);
  }

  // Track the thread id and nothing else. `load` reads `loadedFor` before its
  // first await and then writes it, so calling it tracked makes this effect
  // depend on its own write: it re-runs, resets `loading` to true, fetches
  // again, and never settles — the pane sits on "Loading artifacts…" while
  // hammering the endpoint. `untrack` keeps the dependency set to `threadId`.
  $effect(() => {
    const target = threadId;
    untrack(() => {
      items = [];
      void target;
      void load();
    });
  });
</script>

<div class="scroll-area h-full px-3 py-2">
  <div class="mx-auto flex max-w-3xl flex-col gap-2 pb-6">
    {#if loading}
      <p class="py-8 text-center text-xs text-muted-foreground">Loading artifacts…</p>
    {:else if error}
      <p class="rounded-md border border-destructive px-3 py-2 text-xs text-destructive">{error}</p>
    {:else if items.length === 0}
      <p class="py-8 text-center text-xs text-muted-foreground">
        Nothing here yet. Files this thread generates or sends will appear in this tab.
      </p>
    {:else}
      {#each items as item (item.path)}
        <div class="rounded-xl border border-border bg-card px-3 py-2.5">
          <div class="flex items-center gap-2">
            <span class="text-muted-foreground"><Icon name="file" /></span>
            <span class="min-w-0 flex-1">
              <span class="block truncate font-mono text-xs">{item.name}</span>
              <span class="block truncate text-xxs text-muted-foreground">
                {formatBytes(item.size_bytes)} · {sourceLabel(item.source)}
              </span>
            </span>
            {#if !item.exists}
              <span class="text-xxs text-muted-foreground">unavailable</span>
            {/if}
          </div>
          <div class="mt-2 flex flex-wrap items-center gap-1.5">
            {#if item.exists}
              <button type="button" onclick={() => (preview = item)} class="row-btn"> Open </button>
              <button
                type="button"
                onclick={() => download(item)}
                class="row-btn row-btn-icon"
                aria-label="Download {item.name}"
              >
                <Icon name="download" />
              </button>
            {/if}
            {#if item.message_sequences.length > 0}
              <button type="button" onclick={() => ongoto(item.message_sequences[0])} class="row-btn">
                Go to message
              </button>
            {:else}
              <span class="text-xxs text-muted-foreground">not linked to a message</span>
            {/if}
          </div>
        </div>
      {/each}
    {/if}
  </div>
</div>

{#if preview}
  <ArtifactPreview threadId={threadId} artifact={preview} onclose={() => (preview = null)} />
{/if}

<style>
  .row-btn {
    display: inline-flex;
    align-items: center;
    border-radius: 6px;
    border: 1px solid var(--color-border);
    padding: 0.25rem 0.625rem;
    font-size: 0.75rem;
    color: var(--color-foreground);
  }

  .row-btn-icon {
    padding: 0.25rem 0.375rem;
  }

  .row-btn:hover {
    background: var(--color-accent);
  }
</style>
