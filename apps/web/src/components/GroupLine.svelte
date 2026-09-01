<script lang="ts">
  import type { GroupNode } from "$lib/transcript";
  import { selectionChanged } from "$lib/telegram";
  import ToolLine from "./ToolLine.svelte";

  interface Props {
    node: GroupNode;
  }

  let { node }: Props = $props();
  let open = $state(false);

  let failedCount = $derived(node.items.filter((i) => i.summary.failed).length);

  function toggle() {
    open = !open;
    selectionChanged();
  }
</script>

<div>
  <button type="button" onclick={toggle} aria-expanded={open} class="line">
    <span class="label">{node.label}</span>
    {#if failedCount}
      <span class="failed">{failedCount} failed</span>
    {/if}
    <svg viewBox="0 0 24 24" class="chev" class:open aria-hidden="true">
      <path
        fill="none"
        stroke="currentColor"
        stroke-width="1.75"
        stroke-linecap="round"
        stroke-linejoin="round"
        d="m9 18 6-6-6-6"
      />
    </svg>
  </button>

  {#if open}
    <div class="items">
      {#each node.items as item (item.key)}
        <ToolLine node={item} nested />
      {/each}
    </div>
  {/if}
</div>

<style>
  .line {
    display: flex;
    width: 100%;
    align-items: baseline;
    gap: 0.4rem;
    padding: 0.15rem 0;
    text-align: left;
    font-size: 0.8125rem;
    line-height: 1.5;
    color: var(--color-muted-foreground);
  }

  .label {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .failed {
    flex: none;
    font-family: var(--font-mono);
    font-size: 0.6875rem;
    color: var(--color-diff-del-fg);
  }

  .chev {
    flex: none;
    margin-left: auto;
    width: 0.875rem;
    height: 0.875rem;
    opacity: 0.6;
    transition: rotate 200ms ease-out;
  }
  .chev.open {
    rotate: 90deg;
  }
  @media (prefers-reduced-motion: reduce) {
    .chev {
      transition: none;
    }
  }

  .items {
    display: flex;
    flex-direction: column;
    padding: 0.15rem 0 0.25rem;
  }
</style>
