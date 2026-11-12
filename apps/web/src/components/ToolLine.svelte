<script lang="ts">
  import type { ToolNode } from "$lib/transcript";
  import { selectionChanged } from "$lib/telegram";
  import ToolDetail from "./ToolDetail.svelte";

  interface Props {
    node: ToolNode;
    /** Indented when nested inside a folded group. */
    nested?: boolean;
  }

  let { node, nested = false }: Props = $props();
  let open = $state(false);

  let s = $derived(node.summary);
  /** Finished, but its round is not in the thread file yet. */
  let settling = $derived(!!node.running?.completed_at);

  function toggle() {
    open = !open;
    selectionChanged();
  }

  /** The view's row fact; bare calls without a view fall back to `failed`. */
  let hint = $derived(
    node.view?.hint ?? (s.failed ? { text: "failed", bad: true } : undefined),
  );
</script>

<div class:nested>
  <button type="button" onclick={toggle} aria-expanded={open} class="line">
    {#if node.running && !settling}
      <span class="dot" aria-hidden="true"></span>
    {/if}

    <span class="verb" class:shell={s.verb === "$"}>{s.verb}</span>

    {#if s.subject}
      <span class="subject" class:mono={s.mono}>{s.subject}</span>
    {/if}

    {#if s.additions}<span class="add">+{s.additions}</span>{/if}
    {#if s.deletions}<span class="del">-{s.deletions}</span>{/if}

    {#if node.running}
      {#if node.running.ok === false}<span class="del">failed</span>{/if}
      <span class="meta">{node.running.running_for}</span>
    {:else if hint && (hint.bad || (!s.additions && !s.deletions))}
      <span class={hint.bad ? "del" : "meta"}>{hint.text}</span>
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
    <ToolDetail {node} />
  {/if}
</div>

<style>
  .nested {
    padding-left: 0.75rem;
    border-left: 1px solid var(--color-border);
    margin-left: 0.25rem;
  }

  /* Deliberately borderless and low-contrast: activity should recede until
     asked for, so the prose answer stays the focus. */
  .line {
    display: flex;
    width: 100%;
    align-items: baseline;
    gap: 0.4rem;
    padding: 0.15rem 0;
    text-align: left;
    font-size: 0.8125rem;
    color: var(--color-muted-foreground);
    line-height: 1.5;
  }

  .verb {
    flex: none;
  }
  .verb.shell {
    font-family: var(--font-mono);
    color: var(--color-diff-del-fg);
  }

  .subject {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--color-foreground);
    font-weight: 500;
  }
  .subject.mono {
    font-family: var(--font-mono);
    font-size: 0.75rem;
    font-weight: 400;
  }

  .add {
    flex: none;
    font-family: var(--font-mono);
    font-size: 0.6875rem;
    color: var(--color-diff-add-fg);
  }
  .del {
    flex: none;
    font-family: var(--font-mono);
    font-size: 0.6875rem;
    color: var(--color-diff-del-fg);
  }

  .meta {
    flex: none;
    font-family: var(--font-mono);
    font-size: 0.6875rem;
    opacity: 0.8;
  }

  .dot {
    flex: none;
    width: 0.375rem;
    height: 0.375rem;
    border-radius: 9999px;
    background: var(--color-warning);
    animation: pulse 1.2s ease-in-out infinite;
  }
  @keyframes pulse {
    50% {
      opacity: 0.3;
    }
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
    .dot {
      animation: none;
    }
  }

</style>
