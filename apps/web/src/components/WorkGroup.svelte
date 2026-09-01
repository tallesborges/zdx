<script lang="ts">
  import type { WorkNode, UsageNode } from "$lib/transcript";
  import { formatDuration } from "$lib/transcript";
  import { selectionChanged } from "$lib/telegram";
  import ToolLine from "./ToolLine.svelte";
  import GroupLine from "./GroupLine.svelte";
  import Collapse from "./Collapse.svelte";

  interface Props {
    node: WorkNode;
  }

  let { node }: Props = $props();

  // Live work expands itself so a running turn is visible without a tap, but an
  // explicit toggle always wins from then on.
  let userOpen = $state<boolean | null>(null);
  let open = $derived(userOpen ?? node.running);

  let label = $derived.by(() => {
    if (node.running) return "Working";
    const d = formatDuration(node.durationMs);
    return d ? `Worked for ${d}` : "Worked";
  });

  function toggle() {
    userOpen = !open;
    selectionChanged();
  }

  function tokens(n: number): string {
    if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(2)}M`;
    if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
    return String(n);
  }

  function usageDetail(u: UsageNode["activity"]): string {
    const total = tokens(u.input_tokens + u.output_tokens);
    return u.model ? `${total} tokens · ${u.model}` : `${total} tokens`;
  }
</script>

<div class="wrap">
  <!-- A rule-flanked divider, so a collapsed turn reads as a seam rather than
       a control competing with the answer. -->
  <button type="button" onclick={toggle} aria-expanded={open} class="divider">
    <span class="rule"></span>
    <span class="label" class:live={node.running}>{label}</span>
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
    <span class="rule"></span>
  </button>

  {#if open}
    <div class="items">
      {#each node.items as item (item.key)}
        {#if item.kind === "tool"}
          <ToolLine node={item} />
        {:else if item.kind === "group"}
          <GroupLine node={item} />
        {:else if item.kind === "reasoning"}
          <Collapse label="Thinking" detail={item.redacted ? "redacted" : ""}>
            {#if item.redacted}
              <p class="m-0 text-xs text-muted-foreground italic">
                Reasoning was redacted by the provider.
              </p>
            {:else}
              <div
                class="text-[0.8125rem] leading-relaxed whitespace-pre-wrap text-muted-foreground"
              >
                {item.text}
              </div>
            {/if}
          </Collapse>
        {:else}
          <Collapse label="usage" detail={usageDetail(item.activity)}>
            <dl class="grid grid-cols-2 gap-x-4 gap-y-1 font-mono text-xxs">
              <dt class="text-muted-foreground">input</dt>
              <dd class="m-0 text-right">{item.activity.input_tokens.toLocaleString()}</dd>
              <dt class="text-muted-foreground">output</dt>
              <dd class="m-0 text-right">{item.activity.output_tokens.toLocaleString()}</dd>
              <dt class="text-muted-foreground">cache read</dt>
              <dd class="m-0 text-right">{item.activity.cache_read_tokens.toLocaleString()}</dd>
              <dt class="text-muted-foreground">cache write</dt>
              <dd class="m-0 text-right">{item.activity.cache_write_tokens.toLocaleString()}</dd>
            </dl>
          </Collapse>
        {/if}
      {/each}
    </div>
  {/if}
</div>

<style>
  .wrap {
    margin: 0.25rem 0;
  }

  .divider {
    display: flex;
    width: 100%;
    align-items: center;
    gap: 0.5rem;
    padding: 0.25rem 0;
    color: var(--color-muted-foreground);
  }

  .rule {
    flex: 1;
    height: 1px;
    background: var(--color-border);
  }

  .label {
    flex: none;
    font-size: 0.8125rem;
  }
  .label.live {
    color: var(--color-warning);
  }

  .chev {
    flex: none;
    width: 0.875rem;
    height: 0.875rem;
    opacity: 0.7;
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
    padding: 0.25rem 0;
  }
</style>
