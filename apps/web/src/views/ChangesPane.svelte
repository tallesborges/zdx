<script lang="ts">
  import { api, ApiError } from "$lib/api";
  import type { GitDiffResponse, GitFile, GitFileKind, GitResponse } from "$lib/types";
  import DiffView from "../components/DiffView.svelte";
  import { selectionChanged } from "$lib/telegram";

  interface Props {
    id: string;
    /** Owned by ThreadView, which needs it for the dirty-tree dot. */
    data: GitResponse | null;
    error?: string;
  }

  let { id, data, error = "" }: Props = $props();

  let selected = $state<{ kind: GitFileKind; path: string } | null>(null);
  let diff = $state<GitDiffResponse | null>(null);
  let diffError = $state("");
  let diffLoading = $state(false);

  async function openDiff(kind: GitFileKind, path: string) {
    selectionChanged();
    selected = { kind, path };
    diff = null;
    diffError = "";
    diffLoading = true;
    try {
      diff = await api.gitDiff(id, kind, path);
    } catch (e) {
      diffError = e instanceof ApiError ? e.message : String(e);
    } finally {
      diffLoading = false;
    }
  }

  function close() {
    selected = null;
    diff = null;
    diffError = "";
  }

  const groups: Array<[GitFileKind, string]> = [
    ["staged", "Staged"],
    ["unstaged", "Unstaged"],
    ["untracked", "Untracked"],
  ];

  function filesFor(kind: GitFileKind): GitFile[] {
    return data ? data.files[kind] : [];
  }
</script>

<div class="scroll-area flex-1 px-3 py-3">
  {#if error}
    <p class="rounded-md border border-destructive px-3 py-2 text-xs text-destructive">{error}</p>
  {:else if !data}
    <p class="py-8 text-center text-xs text-muted-foreground">Loading…</p>
  {:else}
    <div class="mx-auto flex max-w-3xl flex-col gap-3 pb-6">
      <div class="rounded-md border border-border bg-card px-2.5 py-2">
        <div class="flex items-baseline gap-2 font-mono text-xs">
          <span class="font-medium">{data.repository.branch}</span>
          {#if data.repository.head}
            <span class="text-muted-foreground">{data.repository.head}</span>
          {/if}
          <span class="ml-auto text-xxs text-muted-foreground">
            {#if data.repository.ahead}<span class="text-success">↑{data.repository.ahead}</span>{/if}
            {#if data.repository.behind}<span class="text-warning">↓{data.repository.behind}</span>{/if}
            {#if !data.repository.ahead && !data.repository.behind}in sync{/if}
          </span>
        </div>
        <p class="m-0 mt-0.5 truncate font-mono text-xxs text-muted-foreground" dir="rtl">
          {data.repository.root}
        </p>
      </div>

      {#if data.repository.clean}
        <p class="rounded-md border border-border bg-card px-2.5 py-2 text-xs text-muted-foreground">
          Working tree clean.
        </p>
      {/if}

      {#each groups as [kind, label] (kind)}
        {@const files = filesFor(kind)}
        {#if files.length}
          <section>
            <h2 class="sec">{label} · {files.length}</h2>
            <div class="flex flex-col gap-1">
              {#each files as file (file.path)}
                <button
                  type="button"
                  onclick={() => openDiff(kind, file.path)}
                  class="flex w-full items-center gap-2 rounded-md border border-border bg-card px-2.5 py-1.5 text-left hover:bg-accent"
                >
                  <span
                    class="w-3 shrink-0 text-center font-mono text-xxs"
                    class:text-success={kind === "staged"}
                    class:text-warning={kind === "unstaged"}
                    class:text-muted-foreground={kind === "untracked"}>{file.status}</span
                  >
                  <span class="truncate font-mono text-xs" dir="rtl">{file.path}</span>
                </button>
              {/each}
            </div>
          </section>
        {/if}
      {/each}

      {#if data.commits.length}
        <section>
          <h2 class="sec">Recent commits</h2>
          <div class="flex flex-col gap-1">
            {#each data.commits as c (c.hash)}
              <div class="rounded-md border border-border bg-card px-2.5 py-1.5">
                <p class="m-0 truncate text-xs">{c.subject}</p>
                <p class="m-0 font-mono text-xxs text-muted-foreground">
                  {c.short_hash} · {c.author} · {c.relative_time}
                </p>
              </div>
            {/each}
          </div>
        </section>
      {/if}
    </div>
  {/if}
</div>

{#if selected}
  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
  <div class="fixed inset-0 z-20 flex flex-col bg-black/40" onclick={close}>
    <div class="flex-1"></div>
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div
      class="app-frame flex max-h-[85dvh] min-h-[40dvh] flex-col"
      style="padding-bottom: var(--tg-safe-bottom)"
      onclick={(e) => e.stopPropagation()}
    >
      <div class="flex items-center gap-2 border-b border-border px-3 py-2">
        <span class="min-w-0 flex-1 truncate font-mono text-xs" dir="rtl">{selected.path}</span>
        <button
          type="button"
          onclick={close}
          aria-label="Close"
          class="rounded-sm p-1 text-muted-foreground hover:bg-accent"
        >
          <svg viewBox="0 0 24 24" class="size-4" aria-hidden="true">
            <path
              fill="none"
              stroke="currentColor"
              stroke-width="1.75"
              stroke-linecap="round"
              d="M18 6 6 18M6 6l12 12"
            />
          </svg>
        </button>
      </div>

      <div class="scroll-area flex-1 p-3">
        {#if diffLoading}
          <p class="py-6 text-center text-xs text-muted-foreground">Loading diff…</p>
        {:else if diffError}
          <p class="text-xs text-destructive">{diffError}</p>
        {:else if diff}
          <DiffView
            content={diff.content}
            path={diff.path}
            truncated={diff.truncated}
            limitBytes={diff.limit_bytes}
          />
        {/if}
      </div>
    </div>
  </div>
{/if}

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
