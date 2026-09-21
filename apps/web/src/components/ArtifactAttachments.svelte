<script lang="ts">
  import { onDestroy } from "svelte";
  import { api, formatBytes } from "$lib/api";
  import type { ArtifactAttachment } from "$lib/types";
  import ArtifactPreview from "./ArtifactPreview.svelte";
  import Icon from "./Icon.svelte";

  interface Props {
    threadId: string;
    attachments: ArtifactAttachment[];
  }

  let { threadId, attachments }: Props = $props();

  let urls = $state(new Map<string, string>());
  let failed = $state(new Set<string>());
  let preview = $state<ArtifactAttachment | null>(null);

  function objectUrlFor(path: string): string | null {
    return urls.get(path) ?? null;
  }

  $effect(() => {
    const wanted = attachments.filter(
      (a) => a.exists && (a.kind === "image" || a.kind === "audio") && !urls.has(a.path),
    );
    if (wanted.length === 0) return;
    let cancelled = false;
    (async () => {
      for (const item of wanted) {
        try {
          const blob = await api.artifactBlob(threadId, item.path);
          if (cancelled) return;
          const objectUrl = URL.createObjectURL(blob);
          urls = new Map(urls).set(item.path, objectUrl);
        } catch {
          if (!cancelled) failed = new Set(failed).add(item.path);
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  });

  onDestroy(() => {
    for (const objectUrl of urls.values()) URL.revokeObjectURL(objectUrl);
  });

  async function download(item: ArtifactAttachment) {
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
</script>

<div class="flex flex-col gap-2">
  {#each attachments as item (item.path)}
    {#if !item.exists}
      <div class="rounded-lg border border-border px-3 py-2 text-xs text-muted-foreground">
        <span class="inline-flex items-center gap-1.5">
          <Icon name="file" />
          <span class="truncate font-mono">{item.name}</span>
        </span>
        <span class="mt-0.5 block">No longer available on the bot host.</span>
      </div>
    {:else if item.kind === "image"}
      {@const src = objectUrlFor(item.path)}
      {#if src}
        <button type="button" onclick={() => (preview = item)} class="thumb-btn" aria-label="Open {item.name}">
          <img {src} alt={item.name} loading="lazy" class="thumb" />
        </button>
      {:else if failed.has(item.path)}
        <button type="button" onclick={() => (preview = item)} class="file-card">
          <Icon name="file" />
          <span class="min-w-0 flex-1 truncate text-left font-mono text-xs">{item.name}</span>
          <span class="text-xxs text-muted-foreground">{formatBytes(item.size_bytes)}</span>
        </button>
      {:else}
        <div class="thumb-loading">Loading image…</div>
      {/if}
    {:else if item.kind === "audio"}
      {@const src = objectUrlFor(item.path)}
      {#if src}
        <audio controls src={src} class="w-full"></audio>
      {:else if failed.has(item.path)}
        <button type="button" onclick={() => download(item)} class="file-card">
          <Icon name="file" />
          <span class="min-w-0 flex-1 truncate text-left font-mono text-xs">{item.name}</span>
          <span class="text-xxs text-muted-foreground">{formatBytes(item.size_bytes)}</span>
        </button>
      {:else}
        <div class="thumb-loading">Loading audio…</div>
      {/if}
    {:else}
      <div class="file-card-static">
        <Icon name="file" />
        <span class="min-w-0 flex-1">
          <span class="block truncate font-mono text-xs">{item.name}</span>
          <span class="block text-xxs text-muted-foreground">
            {item.kind === "html" ? "HTML" : item.mime} · {formatBytes(item.size_bytes)}
          </span>
        </span>
        <button type="button" onclick={() => (preview = item)} class="card-btn">Open</button>
        <button type="button" onclick={() => download(item)} class="card-btn" aria-label="Download {item.name}">
          <Icon name="download" />
        </button>
      </div>
    {/if}
  {/each}
</div>

{#if preview}
  <ArtifactPreview threadId={threadId} artifact={preview} onclose={() => (preview = null)} />
{/if}

<style>
  .thumb-btn {
    display: block;
    overflow: hidden;
    border-radius: 10px;
    border: 1px solid var(--color-border);
    background: var(--color-accent);
    padding: 0;
  }

  .thumb {
    display: block;
    width: 100%;
    height: auto;
    max-height: 16rem;
    object-fit: cover;
  }

  .thumb-loading {
    border-radius: 10px;
    border: 1px solid var(--color-border);
    padding: 0.75rem;
    font-size: 0.75rem;
    color: var(--color-muted-foreground);
  }

  .file-card {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    border-radius: 10px;
    border: 1px solid var(--color-border);
    background: var(--color-card);
    padding: 0.625rem 0.75rem;
    color: var(--color-foreground);
  }

  .file-card-static {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    border-radius: 10px;
    border: 1px solid var(--color-border);
    background: var(--color-card);
    padding: 0.625rem 0.75rem;
    color: var(--color-foreground);
  }

  .card-btn {
    flex: none;
    border-radius: 6px;
    border: 1px solid var(--color-border);
    padding: 0.25rem 0.625rem;
    font-size: 0.75rem;
    color: var(--color-foreground);
  }

  .card-btn:hover,
  .file-card:hover {
    background: var(--color-accent);
  }
</style>
