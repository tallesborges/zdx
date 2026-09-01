<script lang="ts">
  import type { Snippet } from "svelte";
  import { selectionChanged } from "$lib/telegram";

  interface Props {
    label: string;
    detail?: string;
    children: Snippet;
  }

  let { label, detail = "", children }: Props = $props();

  let open = $state(false);

  function toggle() {
    open = !open;
    selectionChanged();
  }
</script>

<div>
  <!-- Matches ToolLine: borderless and low-contrast, so agent activity recedes
       until asked for. -->
  <button type="button" onclick={toggle} aria-expanded={open} class="line">
    <span class="label">{label}</span>
    {#if detail}
      <span class="detail">{detail}</span>
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
    <div class="body">
      {@render children()}
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
    flex: none;
  }

  .detail {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: var(--font-mono);
    font-size: 0.6875rem;
    opacity: 0.85;
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

  .body {
    padding: 0.25rem 0 0.5rem 0.75rem;
    border-left: 1px solid var(--color-border);
    margin-left: 0.25rem;
  }
</style>
