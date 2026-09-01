<script lang="ts">
  import { selectionChanged } from "$lib/telegram";
  import Icon from "./Icon.svelte";
  import type { IconName } from "./Icon.svelte";

  interface Tab<T extends string> {
    id: T;
    label: string;
    icon?: IconName;
    /** Overlays a dot on the icon — "there is something in here". */
    dot?: boolean;
  }

  interface Props<T extends string> {
    tabs: Tab<T>[];
    active: T;
    onselect: (id: T) => void;
  }

  let { tabs, active, onselect }: Props<string> = $props();

  function pick(id: string) {
    if (id === active) return;
    selectionChanged();
    onselect(id);
  }
</script>

<!-- Horizontally scrollable so tabs can grow without wrapping or shrinking. -->
<div class="strip" role="tablist">
  {#each tabs as tab (tab.id)}
    <button
      type="button"
      role="tab"
      aria-selected={tab.id === active}
      data-active={tab.id === active}
      onclick={() => pick(tab.id)}
      class="tab"
    >
      {#if tab.icon}
        <span class="ico">
          <Icon name={tab.icon} />
          {#if tab.dot}
            <span class="dot"></span>
          {/if}
        </span>
      {/if}

      {tab.label}
    </button>
  {/each}
</div>

<style>
  .strip {
    display: flex;
    gap: 0.125rem;
    padding: 0.25rem 0.5rem 0.375rem;
    overflow-x: auto;
    scrollbar-width: none;
    -webkit-overflow-scrolling: touch;
    border-bottom: 1px solid var(--color-border);
    /* Keep the active tab reachable when the strip overflows. */
    scroll-snap-type: x proximity;
  }
  .strip::-webkit-scrollbar {
    display: none;
  }

  .tab {
    display: inline-flex;
    flex: none;
    align-items: center;
    gap: 0.375rem;
    padding: 0.3rem 0.6rem;
    border-radius: 6px;
    scroll-snap-align: start;
    font-size: 0.8125rem;
    font-weight: 500;
    white-space: nowrap;
    color: var(--color-muted-foreground);
    transition:
      background-color 150ms ease-out,
      color 150ms ease-out;
  }

  .tab[data-active="true"] {
    background: var(--color-accent);
    color: var(--color-foreground);
  }

  @media (prefers-reduced-motion: reduce) {
    .tab {
      transition: none;
    }
  }

  .ico {
    position: relative;
    display: inline-flex;
  }

  .dot {
    position: absolute;
    top: -1px;
    right: -2px;
    width: 0.3125rem;
    height: 0.3125rem;
    border-radius: 9999px;
    background: var(--color-link);
    /* Ring in the panel colour so the dot reads as an overlay on the glyph. */
    box-shadow: 0 0 0 1.5px var(--color-surface);
  }
</style>
