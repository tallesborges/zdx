<script lang="ts">
  import { onDestroy, untrack } from "svelte";
  import { api, ApiError, formatBytes } from "$lib/api";
  import { draftKey, loadDraftRecord } from "$lib/draftStore";
  import { confirmAction, telegramUserId } from "$lib/telegram";
  import { decodeTextPreview, TEXT_FETCH_LIMIT, TEXT_RENDER_LIMIT } from "$lib/textPreview";
  import type { TextPreview } from "$lib/textPreview";
  import type { ArtifactAttachment, ArtifactImageCopy } from "$lib/types";
  import ArtifactAnnotator from "./ArtifactAnnotator.svelte";
  import Icon from "./Icon.svelte";
  import Markdown from "./Markdown.svelte";
  import TextView from "./TextView.svelte";

  interface Props {
    threadId: string;
    artifact: ArtifactAttachment;
    onclose: () => void;
  }

  let { threadId, artifact, onclose }: Props = $props();

  let url = $state<string | null>(null);
  /** HTML bytes as served, for the annotator (it injects its helper into a copy). */
  let htmlBlob = $state<Blob | null>(null);
  /** An image's canonical copy from the server: what is shown, marked, and
   *  sent to the agent. The app never re-encodes or re-orients it. */
  let imageCopy = $state<ArtifactImageCopy | null>(null);
  /** Why an image is shown as a preview only (the server's reason). */
  let passiveReason = $state<string | null>(null);
  /** Markdown, plain-text, and code files: the decoded start of the file. */
  let textPreview = $state<TextPreview | null>(null);
  /** Markdown shows rendered by default; Source shows the file as text. */
  let markdownSource = $state(false);
  /** Size of the fetched text file (the listed size can be older). */
  let textTotal = $state(0);
  const isText = $derived(artifact.kind === "markdown" || artifact.kind === "text");
  /** Text files this large are not fetched for a preview, only downloaded. */
  const textTooLarge = $derived(isText && artifact.size_bytes > TEXT_FETCH_LIMIT);
  let error = $state("");
  let loading = $state(true);
  /** The annotator is shown: HTML, or an image with its canonical copy. */
  const annotating = $derived(
    artifact.exists &&
      !loading &&
      !error &&
      ((artifact.kind === "html" && htmlBlob !== null) || (artifact.kind === "image" && imageCopy !== null)),
  );
  /** The annotator has an open note, a recording, or a stroke closing would lose. */
  let unsaved = $state(false);

  /** Saved notes survive closing; only unsaved editor work asks first. */
  async function requestClose() {
    if (unsaved && !(await confirmAction("Discard the note you are editing?"))) return;
    onclose();
  }

  /** Fetches the bytes once and keeps the blob URL for the open preview. */
  async function fetchBytes(): Promise<string | null> {
    const existing = untrack(() => url);
    if (existing) return existing;
    try {
      const blob = await api.artifactBlob(threadId, artifact.path);
      const objectUrl = URL.createObjectURL(blob);
      url = objectUrl;
      return objectUrl;
    } catch (e) {
      error = e instanceof ApiError ? e.message : String(e);
      return null;
    } finally {
      loading = false;
    }
  }

  async function save() {
    const objectUrl = await fetchBytes();
    if (!objectUrl) return;
    const anchor = document.createElement("a");
    anchor.href = objectUrl;
    anchor.download = artifact.name;
    document.body.appendChild(anchor);
    anchor.click();
    anchor.remove();
  }

  /** The canonical copy when the server can make one; otherwise the original,
   *  shown as a passive preview with the server's reason. */
  async function loadImage(target: string, path: string): Promise<{ copy: ArtifactImageCopy | null; reason: string | null; blob: Blob | null }> {
    try {
      return { copy: await api.artifactImage(target, path), reason: null, blob: null };
    } catch (e) {
      if (!(e instanceof ApiError) || e.status !== 422) throw e;
      // Notes made while the image could be annotated stay on the device
      // untouched; say so instead of hiding them.
      const record = await loadDraftRecord(draftKey(telegramUserId(), target, path)).catch(() => null);
      const kept = record?.drafts.length ?? 0;
      const notes =
        kept > 0
          ? ` Your ${kept === 1 ? "unsent note on it is" : `${kept} unsent notes on it are`} kept on this device.`
          : "";
      return {
        copy: null,
        reason: `${e.message} It is shown as a preview only.${notes}`,
        blob: await api.artifactBlob(target, path),
      };
    }
  }

  // Tracks the artifact identity only; the fetch writes `url`/`loading`/`error`
  // and must not feed back into this effect's dependency set.
  $effect(() => {
    const target = threadId;
    const path = artifact.path;
    const kind = artifact.kind;
    const exists = artifact.exists;
    const size = artifact.size_bytes;

    return untrack(() => {
      const previous = url;
      url = null;
      htmlBlob = null;
      imageCopy = null;
      passiveReason = null;
      textPreview = null;
      markdownSource = false;
      if (previous) URL.revokeObjectURL(previous);
      loading = true;
      error = "";
      const text = kind === "markdown" || kind === "text";
      // Images, audio, HTML, and text need bytes to render. Other kinds (and
      // very large text) fetch lazily through the Download button instead.
      if (kind === "other" || !exists || (text && size > TEXT_FETCH_LIMIT)) {
        loading = false;
        return;
      }
      let cancelled = false;
      const load =
        kind === "image"
          ? loadImage(target, path)
          : api.artifactBlob(target, path).then((blob) => ({ copy: null, reason: null, blob }));
      void load
        .then(async ({ copy, reason, blob }) => {
          if (cancelled) return;
          passiveReason = reason;
          if (copy) imageCopy = copy;
          else if (blob) {
            if (kind === "html") htmlBlob = blob;
            if (text) {
              const head = new Uint8Array(await blob.slice(0, TEXT_RENDER_LIMIT + 1).arrayBuffer());
              if (cancelled) return;
              textPreview = decodeTextPreview(head);
              textTotal = blob.size;
            }
            url = URL.createObjectURL(blob);
          }
        })
        .catch((e: unknown) => {
          if (!cancelled) error = e instanceof ApiError ? e.message : String(e);
        })
        .finally(() => {
          if (!cancelled) loading = false;
        });
      return () => {
        cancelled = true;
      };
    });
  });

  onDestroy(() => {
    if (url) URL.revokeObjectURL(url);
  });

  function backdropKey(event: KeyboardEvent) {
    // Escape in a text field cancels the field, not the whole preview.
    const target = event.target as HTMLElement | null;
    if (target?.closest("input, textarea, [contenteditable]")) return;
    if (event.key === "Escape") void requestClose();
  }
</script>

<svelte:window onkeydown={backdropKey} />

<div
  class="preview-backdrop"
  role="presentation"
  onclick={(e) => {
    if (e.target === e.currentTarget) void requestClose();
  }}
>
  <div class="preview-sheet" class:fill={annotating} role="dialog" aria-label={artifact.name}>
    <header class="flex shrink-0 items-center gap-2 border-b border-border px-3 py-2">
      <div class="min-w-0 flex-1">
        <p class="m-0 truncate text-sm font-semibold">{artifact.name}</p>
        <p class="m-0 truncate font-mono text-xxs text-muted-foreground">{artifact.mime}</p>
      </div>
      {#if artifact.kind === "markdown" && textPreview && !textPreview.binary}
        <div class="seg" role="group" aria-label="Markdown view">
          <button type="button" class:on={!markdownSource} aria-pressed={!markdownSource} onclick={() => (markdownSource = false)}>Preview</button>
          <button type="button" class:on={markdownSource} aria-pressed={markdownSource} onclick={() => (markdownSource = true)}>Source</button>
        </div>
      {/if}
      {#if artifact.kind === "other" || (isText && artifact.exists)}
        <button type="button" onclick={save} class="preview-btn" aria-label="Download {artifact.name}">
          <Icon name="download" />
          {#if artifact.kind === "other"}<span>Download</span>{/if}
        </button>
      {/if}
      <button type="button" onclick={requestClose} aria-label="Close preview" class="preview-icon-btn">
        <Icon name="x" />
      </button>
    </header>

    <div class="preview-body">
      {#if !artifact.exists}
        <p class="py-8 text-center text-xs text-muted-foreground">
          This file is no longer on the bot host, so there is nothing to preview.
        </p>
      {:else if loading}
        <p class="py-8 text-center text-xs text-muted-foreground">Loading preview…</p>
      {:else if error}
        <p class="mx-3 rounded-md border border-destructive px-3 py-2 text-xs text-destructive">
          {error}
        </p>
      {:else if artifact.kind === "html" && htmlBlob}
        <ArtifactAnnotator {threadId} {artifact} blob={htmlBlob} bind:unsaved />
      {:else if artifact.kind === "image" && imageCopy}
        <ArtifactAnnotator {threadId} {artifact} blob={imageCopy.blob} image={imageCopy} bind:unsaved />
      {:else if artifact.kind === "image" && url}
        {#if passiveReason}<p class="passive-note">{passiveReason}</p>{/if}
        <img src={url} alt={artifact.name} class="preview-media" />
      {:else if artifact.kind === "audio" && url}
        <div class="p-4">
          <audio controls src={url} class="w-full"></audio>
        </div>
      {:else if isText && textPreview && !textPreview.binary}
        {#if textPreview.truncated}
          <p class="passive-note">
            Showing the first {formatBytes(textPreview.shownBytes)} of {formatBytes(textTotal)}. Download
            the file to see all of it.
          </p>
        {/if}
        {#if artifact.kind === "markdown" && !markdownSource}
          <div class="px-4 py-3">
            <Markdown source={textPreview.text} />
          </div>
        {:else}
          <TextView text={textPreview.text} path={artifact.name} />
        {/if}
      {:else}
        <div class="flex flex-col items-center gap-2 py-8">
          <p class="m-0 text-xs text-muted-foreground">
            {#if textTooLarge}
              This file is too large to preview ({formatBytes(artifact.size_bytes)}).
            {:else if textPreview?.binary}
              This file does not contain text, so there is no inline preview.
            {:else}
              No inline preview for this file type.
            {/if}
          </p>
          <button type="button" onclick={save} class="preview-btn">
            <Icon name="download" />
            <span>Download</span>
          </button>
        </div>
      {/if}
    </div>
  </div>
</div>

<style>
  .preview-backdrop {
    position: fixed;
    inset: 0;
    z-index: 60;
    display: flex;
    align-items: flex-end;
    justify-content: center;
    background: rgb(0 0 0 / 0.45);
  }

  .preview-sheet {
    display: flex;
    flex-direction: column;
    width: 100%;
    max-height: calc(100dvh - var(--tg-safe-top) - var(--tg-safe-bottom) - 0.5rem);
    margin-bottom: var(--tg-safe-bottom);
    overflow: hidden;
    background: var(--color-surface);
    border-radius: 12px 12px 0 0;
    box-shadow: 0 -8px 32px rgb(0 0 0 / 0.3);
  }

  .preview-body {
    min-height: 0;
    flex: 1;
    overflow: auto;
  }

  /* The annotator fills the sheet, and the sheet fills the viewport below a
     small top gap, so the page gets all the space the overlay has. */
  .preview-sheet.fill {
    height: calc(100dvh - var(--tg-safe-top) - var(--tg-safe-bottom) - 0.5rem);
  }

  .preview-sheet.fill .preview-body {
    display: flex;
    flex-direction: column;
  }

  .preview-media {
    display: block;
    width: 100%;
    height: auto;
    max-height: calc(100dvh - 12rem);
    object-fit: contain;
    background: var(--color-accent);
  }

  .passive-note {
    margin: 0;
    border-bottom: 1px solid var(--color-border);
    padding: 0.5rem 0.75rem;
    font-size: 0.75rem;
    color: var(--color-muted-foreground);
  }

  .seg {
    display: inline-flex;
    border: 1px solid var(--color-border);
    border-radius: 6px;
    overflow: hidden;
  }

  .seg button {
    padding: 0.25rem 0.5rem;
    font-size: 0.6875rem;
    color: var(--color-muted-foreground);
  }

  .seg button.on {
    background: var(--color-accent);
    color: var(--color-foreground);
    font-weight: 600;
  }

  .preview-btn {
    display: inline-flex;
    align-items: center;
    gap: 0.375rem;
    border-radius: 6px;
    border: 1px solid var(--color-border);
    background: var(--color-card);
    padding: 0.375rem 0.625rem;
    font-size: 0.75rem;
    color: var(--color-foreground);
  }

  .preview-icon-btn {
    border-radius: 6px;
    padding: 0.375rem;
    color: var(--color-muted-foreground);
  }

  .preview-icon-btn:hover,
  .preview-btn:hover {
    background: var(--color-accent);
  }
</style>
