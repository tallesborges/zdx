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

  /** `uncommitted` | `all` | a commit hash. */
  let scope = $state("uncommitted");
  let scopeOpen = $state(false);
  let scopeFiles = $state<GitFile[]>([]);
  let scopeUntracked = $state<string[]>([]);
  let scopeBaseRef = $state<string | null>(null);
  let scopeAhead = $state(0);
  let scopeError = $state("");
  let scopeLoading = $state(false);

  let selected = $state<{ kind: GitFileKind; path: string } | null>(null);
  let diff = $state<GitDiffResponse | null>(null);
  let diffError = $state("");
  let diffLoading = $state(false);

  let scopeLabel = $derived(
    scope === "uncommitted"
      ? "Uncommitted"
      : scope === "all"
        ? "All Changes"
        : (data?.commits.find((c) => c.hash === scope)?.short_hash ?? scope.slice(0, 8)),
  );

  // History scopes are served by a separate endpoint; `uncommitted` is already
  // in the GitResponse the thread view fetched.
  $effect(() => {
    const current = scope;
    if (current === "uncommitted") {
      scopeFiles = [];
      scopeUntracked = [];
      scopeError = "";
      return;
    }
    let cancelled = false;
    scopeLoading = true;
    scopeError = "";
    api
      .gitScope(id, current)
      .then((res) => {
        if (cancelled) return;
        scopeFiles = res.files;
        scopeUntracked = res.untracked;
        scopeBaseRef = res.base_ref;
        scopeAhead = res.ahead;
      })
      .catch((e) => {
        if (cancelled) return;
        scopeError = e instanceof ApiError ? e.message : String(e);
      })
      .finally(() => {
        if (!cancelled) scopeLoading = false;
      });
    return () => {
      cancelled = true;
    };
  });

  // What "All Changes" is compared against, e.g. "vs master · 3 commits".
  let scopeSubtitle = $derived.by(() => {
    if (scope !== "all" || scopeLoading) return "";
    if (!scopeBaseRef) return "vs HEAD";
    const commits = scopeAhead === 1 ? "1 commit" : `${scopeAhead} commits`;
    return scopeAhead > 0 ? `vs ${scopeBaseRef} · ${commits}` : `vs ${scopeBaseRef}`;
  });

  function pickScope(next: string) {
    selectionChanged();
    scope = next;
    scopeOpen = false;
  }

  /** Untracked files are not in any diff, so they keep their own kind. */
  function kindFor(path: string): GitFileKind {
    if (scope === "all") return scopeUntracked.includes(path) ? "untracked" : "all";
    return "commit";
  }

  async function openDiff(kind: GitFileKind, path: string) {
    selectionChanged();
    selected = { kind, path };
    diff = null;
    diffError = "";
    diffLoading = true;
    try {
      const commit = kind === "commit" ? scope : undefined;
      diff = await api.gitDiff(id, kind, path, commit);
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
    return data && (kind === "staged" || kind === "unstaged" || kind === "untracked")
      ? data.files[kind]
      : [];
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

      <div class="relative">
        <button
          type="button"
          onclick={() => {
            selectionChanged();
            scopeOpen = !scopeOpen;
          }}
          class="flex w-full items-center gap-2 rounded-md border border-border bg-card px-2.5 py-2 text-left hover:bg-accent"
        >
          <span class="text-xs font-medium">{scopeLabel}</span>
          {#if scopeSubtitle}
            <span class="font-mono text-xxs text-muted-foreground">{scopeSubtitle}</span>
          {/if}
          <svg viewBox="0 0 24 24" class="ml-auto size-4 shrink-0 text-muted-foreground" aria-hidden="true">
            <path
              fill="none"
              stroke="currentColor"
              stroke-width="1.75"
              stroke-linecap="round"
              stroke-linejoin="round"
              d="m6 9 6 6 6-6"
            />
          </svg>
        </button>

        {#if scopeOpen}
          <div
            class="absolute inset-x-0 top-full z-10 mt-1 max-h-80 overflow-y-auto rounded-md border border-border bg-surface shadow-lg"
          >
            {#each [["all", "All Changes"], ["uncommitted", "Uncommitted"]] as [value, label] (value)}
              <button
                type="button"
                onclick={() => pickScope(value)}
                class="flex w-full items-center gap-2 px-3 py-2 text-left text-xs hover:bg-accent"
              >
                <span class="flex-1">{label}</span>
                {#if scope === value}<span class="text-muted-foreground">✓</span>{/if}
              </button>
            {/each}

            {#if data.commits.length}
              <div class="border-t border-border"></div>
              {#each data.commits as c (c.hash)}
                <button
                  type="button"
                  onclick={() => pickScope(c.hash)}
                  class="flex w-full items-center gap-2 px-3 py-2 text-left hover:bg-accent"
                >
                  <span class="shrink-0 font-mono text-xxs text-muted-foreground">{c.short_hash}</span>
                  <span class="min-w-0 flex-1 truncate text-xs">{c.subject}</span>
                  {#if scope === c.hash}<span class="text-muted-foreground">✓</span>{/if}
                </button>
              {/each}
            {/if}
          </div>
        {/if}
      </div>

      {#if scope !== "uncommitted"}
        {#if scopeLoading}
          <p class="py-8 text-center text-xs text-muted-foreground">Loading…</p>
        {:else if scopeError}
          <p class="rounded-md border border-destructive px-3 py-2 text-xs text-destructive">
            {scopeError}
          </p>
        {:else if scopeFiles.length === 0}
          <p class="rounded-md border border-border bg-card px-2.5 py-2 text-xs text-muted-foreground">
            No files changed in this scope.
          </p>
        {:else}
          <section>
            <h2 class="sec">{scopeLabel} · {scopeFiles.length}</h2>
            <div class="flex flex-col gap-1">
              {#each scopeFiles as file (file.path)}
                <button
                  type="button"
                  onclick={() => openDiff(kindFor(file.path), file.path)}
                  class="flex w-full items-center gap-2 rounded-md border border-border bg-card px-2.5 py-1.5 text-left hover:bg-accent"
                >
                  <span class="w-3 shrink-0 text-center font-mono text-xxs text-muted-foreground"
                    >{file.status}</span
                  >
                  <span class="truncate font-mono text-xs" dir="rtl">{file.path}</span>
                </button>
              {/each}
            </div>
          </section>
        {/if}
      {:else}
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
      {/if}

      {#if data.commits.length && scope === "uncommitted"}
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
