<script lang="ts">
  import { router } from "$lib/router.svelte";
  import { backButton } from "$lib/telegram";
  import MonitorView from "./views/MonitorView.svelte";
  import ThreadView from "./views/ThreadView.svelte";
  import Drawer from "./components/Drawer.svelte";

  let route = $derived(router.current);
  let menuOpen = $state(false);

  // Telegram's native back button closes the drawer first, then falls back to
  // normal history navigation.
  $effect(() => {
    if (!menuOpen) return;
    return backButton(() => {
      menuOpen = false;
    });
  });
</script>

<!-- Framed shell: shell colour behind, rounded surface panel inset. -->
<div class="flex h-dvh flex-col bg-shell p-1.5 md:p-3">
  <main class="app-frame flex min-h-0 flex-1 flex-col overflow-hidden">
    <!-- Without a boundary a render error (a bad {#each} key, an unexpected null)
         aborts the update and leaves whatever was on screen — typically a
         permanent "Loading…". Surface it instead. -->
    <svelte:boundary>
      {#if route.view === "thread"}
        {#key route.id}
          <ThreadView id={route.id} tab={route.tab} onmenu={() => (menuOpen = true)} />
        {/key}
      {:else}
        <MonitorView section={route.section} onmenu={() => (menuOpen = true)} />
      {/if}

      {#snippet failed(error, reset)}
        <div class="scroll-area flex-1 px-3 py-4">
          <div class="mx-auto flex max-w-3xl flex-col gap-3">
            <p class="m-0 text-sm font-semibold text-destructive">Render failed</p>
            <pre class="err">{String(error)}</pre>
            <div class="flex gap-2">
              <button
                type="button"
                onclick={reset}
                class="rounded-md border border-border px-3 py-1.5 text-xs hover:bg-accent"
              >
                Retry
              </button>
              <button
                type="button"
                onclick={() => router.openMonitor("overview")}
                class="rounded-md border border-border px-3 py-1.5 text-xs hover:bg-accent"
              >
                Back to overview
              </button>
            </div>
          </div>
        </div>
      {/snippet}
    </svelte:boundary>
  </main>
</div>

<Drawer open={menuOpen} onclose={() => (menuOpen = false)} />

<style>
  .err {
    margin: 0;
    max-height: 16rem;
    overflow: auto;
    padding: 0.5rem;
    border-radius: 6px;
    background: var(--color-editor-background);
    color: var(--color-destructive);
    font-family: var(--font-mono);
    font-size: 0.6875rem;
    line-height: 1.5;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
</style>
