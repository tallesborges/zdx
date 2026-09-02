<script lang="ts">
  import {
    router,
    MONITOR_SECTION_LABELS,
    THREAD_TAB_LABELS,
    type MonitorSection,
    type ThreadTab,
  } from "$lib/router.svelte";
  import { selectionChanged } from "$lib/telegram";

  interface Props {
    open: boolean;
    onclose: () => void;
  }

  let { open, onclose }: Props = $props();

  let route = $derived(router.current);

  const threadTabs: ThreadTab[] = ["agent", "transcript", "changes"];
  const monitorSections: MonitorSection[] = [
    "overview",
    "agents",
    "services",
    "usage",
    "config",
    "automations",
  ];

  function goThread(tab: ThreadTab) {
    selectionChanged();
    router.setTab(tab);
    onclose();
  }

  function goThreadList() {
    selectionChanged();
    router.openThreadList();
    onclose();
  }

  function goMonitor(section: MonitorSection) {
    selectionChanged();
    router.openMonitor(section);
    onclose();
  }
</script>

{#if open}
  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
  <div class="fixed inset-0 z-30 flex" role="dialog" aria-modal="true" aria-label="Menu">
    <div class="drawer flex w-64 max-w-[80vw] flex-col bg-surface" data-sidebar="sidebar">
      <div
        class="flex items-center gap-2 border-b border-border px-3 py-2.5"
        style="padding-top: calc(var(--tg-safe-top) + 0.625rem)"
      >
        <span class="font-mono text-sm font-semibold tracking-tight">zdx</span>
        <button
          type="button"
          onclick={onclose}
          aria-label="Close menu"
          class="ml-auto rounded-sm p-1 text-muted-foreground hover:bg-accent"
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

      <div class="scroll-area flex-1 px-2 py-3" data-sidebar="content">
        <div data-sidebar="group">
          <p class="grp">Browse</p>
          <ul data-sidebar="menu">
            <li data-sidebar="menu-item">
              <button
                type="button"
                data-sidebar="menu-button"
                data-active={route.view === "threads"}
                onclick={goThreadList}
                class="item"
              >
                Recent threads
              </button>
            </li>
          </ul>
        </div>

        <div data-sidebar="group" class="mt-4">
          <p class="grp">Thread</p>
          <ul data-sidebar="menu">
            {#each threadTabs as tab (tab)}
              <li data-sidebar="menu-item">
                <button
                  type="button"
                  data-sidebar="menu-button"
                  data-active={route.view === "thread" && route.tab === tab}
                  onclick={() => goThread(tab)}
                  class="item"
                >
                  {THREAD_TAB_LABELS[tab]}
                </button>
              </li>
            {/each}
          </ul>
        </div>

        <div data-sidebar="group" class="mt-4">
          <p class="grp">Monitor</p>
          <ul data-sidebar="menu">
            {#each monitorSections as section (section)}
              <li data-sidebar="menu-item">
                <button
                  type="button"
                  data-sidebar="menu-button"
                  data-active={route.view === "monitor" && route.section === section}
                  onclick={() => goMonitor(section)}
                  class="item"
                >
                  {MONITOR_SECTION_LABELS[section]}
                </button>
              </li>
            {/each}
          </ul>
        </div>
      </div>

      <div
        class="border-t border-border px-3 py-2 font-mono text-xxs text-muted-foreground"
        data-sidebar="footer"
        style="padding-bottom: calc(var(--tg-safe-bottom) + 0.5rem)"
      >
        <span class="block truncate" dir="rtl">{route.id}</span>
      </div>
    </div>

    <div class="scrim flex-1 bg-black/50" onclick={onclose}></div>
  </div>
{/if}

<style>
  .grp {
    margin: 0 0 0.25rem;
    padding: 0 0.5rem;
    font-family: var(--font-mono);
    font-size: 0.625rem;
    font-weight: 500;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--color-muted-foreground);
  }

  ul {
    margin: 0;
    padding: 0;
    list-style: none;
  }

  .item {
    display: block;
    width: 100%;
    padding: 0.4rem 0.5rem;
    border-radius: 6px;
    text-align: left;
    font-size: 0.8125rem;
    color: var(--color-muted-foreground);
  }

  .item[data-active="true"] {
    background: var(--color-accent);
    color: var(--color-foreground);
    font-weight: 500;
  }

  .drawer {
    animation: slide-in 180ms cubic-bezier(0.2, 0.8, 0.2, 1);
  }
  .scrim {
    animation: fade-in 180ms ease-out;
  }

  @keyframes slide-in {
    from {
      transform: translateX(-100%);
    }
  }
  @keyframes fade-in {
    from {
      opacity: 0;
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .drawer,
    .scrim {
      animation: none;
    }
  }
</style>
