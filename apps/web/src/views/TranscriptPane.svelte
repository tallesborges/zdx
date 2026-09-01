<script lang="ts">
  import { tick, untrack } from "svelte";
  import Markdown from "../components/Markdown.svelte";
  import WorkGroup from "../components/WorkGroup.svelte";
  import Icon from "../components/Icon.svelte";
  import { buildNodes } from "$lib/transcript";
  import { haptic } from "$lib/telegram";
  import type { ThreadActivity } from "$lib/types";

  interface Props {
    activity: ThreadActivity[];
  }

  let { activity }: Props = $props();

  let showWork = $state(true);

  let nodes = $derived.by(() => {
    const all = buildNodes(activity);
    return showWork ? all : all.filter((n) => n.kind !== "work");
  });

  let viewport = $state<HTMLDivElement | null>(null);
  let atBottom = $state(true);
  /** New content arrived while the reader was scrolled up. */
  let unseen = $state(false);
  let primed = false;

  /** Within this many px of the end still counts as "following". */
  const BOTTOM_SLACK = 64;

  function measure() {
    const el = viewport;
    if (!el) return;
    const distance = el.scrollHeight - el.scrollTop - el.clientHeight;
    atBottom = distance <= BOTTOM_SLACK;
    if (atBottom) unseen = false;
  }

  async function scrollToBottom(smooth = true) {
    await tick();
    const el = viewport;
    if (!el) return;
    el.scrollTo({ top: el.scrollHeight, behavior: smooth ? "smooth" : "auto" });
    atBottom = true;
    unseen = false;
  }

  function jump() {
    haptic();
    scrollToBottom(true);
  }

  // A thread is a log: open on the newest message rather than the oldest.
  // Later updates only follow along if the reader is still at the end, so
  // scrolling back through history is never yanked away.
  //
  // Only the node count is tracked. Reading the scroll state untracked matters:
  // otherwise every scroll event re-runs this effect and fights the reader.
  $effect(() => {
    const count = nodes.length;
    if (count === 0) return;

    untrack(() => {
      if (!primed) {
        primed = true;
        scrollToBottom(false);
      } else if (atBottom) {
        scrollToBottom(true);
      } else {
        unseen = true;
      }
    });
  });
</script>

<div class="flex items-center gap-2 px-3 pt-2">
  <button
    type="button"
    onclick={() => (showWork = !showWork)}
    class="rounded-sm px-2 py-0.5 font-mono text-xxs text-muted-foreground hover:bg-accent"
  >
    {showWork ? "showing all" : "answers only"}
  </button>
</div>

<div class="relative min-h-0 flex-1">
  <div bind:this={viewport} onscroll={measure} class="scroll-area h-full px-3 py-2">
    {#if nodes.length === 0}
      <p class="py-8 text-center text-xs text-muted-foreground">Nothing to show.</p>
    {:else}
      <div class="mx-auto flex max-w-3xl flex-col gap-2 pb-6">
        {#each nodes as node (node.key)}
          {#if node.kind === "work"}
            <WorkGroup {node} />
          {:else if node.kind === "message"}
            {#if node.activity.speaker === "You"}
              <!-- User turns are right-aligned bubbles, capped so long pastes stay readable. -->
              <div class="flex justify-end">
                <div
                  class="max-w-[min(85%,42rem)] rounded-xl bg-secondary px-3 py-2 text-secondary-foreground"
                >
                  <p class="m-0 text-[0.9375rem] leading-relaxed break-words whitespace-pre-wrap">
                    {node.activity.text}
                  </p>
                </div>
              </div>
            {:else}
              <div class="min-w-0 py-1">
                <Markdown source={node.activity.text} />
              </div>
            {/if}
          {:else if node.activity.type === "notice"}
            {@const n = node.activity}
            <div
              class="rounded-md border px-2.5 py-1.5 text-xs"
              class:border-warning-border={n.kind !== "refusal"}
              class:text-warning={n.kind !== "refusal"}
              class:border-destructive={n.kind === "refusal"}
              class:text-destructive={n.kind === "refusal"}
            >
              <span class="font-mono text-xxs uppercase opacity-70"
                >{n.kind.replace(/_/g, " ")}</span
              >
              <p class="m-0 mt-0.5">{n.message}</p>
            </div>
          {:else}
            <div
              class="rounded-md border border-border px-2.5 py-1.5 text-xs text-muted-foreground"
            >
              <span class="font-mono text-xxs uppercase opacity-70">interrupted</span>
              <p class="m-0 mt-0.5 whitespace-pre-wrap">{node.activity.text}</p>
            </div>
          {/if}
        {/each}
      </div>
    {/if}
  </div>

  {#if !atBottom}
    <button type="button" onclick={jump} aria-label="Jump to latest" class="jump">
      <Icon name="arrow-down" />
      {#if unseen}
        <span class="dot"></span>
      {/if}
    </button>
  {/if}
</div>

<style>
  .jump {
    position: absolute;
    right: 0.75rem;
    /* Clear of the home indicator on a phone. */
    bottom: calc(0.75rem + var(--tg-safe-bottom));
    display: flex;
    align-items: center;
    justify-content: center;
    width: 2rem;
    height: 2rem;
    border-radius: 9999px;
    background: var(--color-surface);
    color: var(--color-foreground);
    box-shadow:
      0 0 0 1px light-dark(#0000001a, #ffffff26),
      0 2px 8px light-dark(#00000026, #00000059);
    animation: pop 140ms cubic-bezier(0.2, 0.8, 0.2, 1);
  }

  .dot {
    position: absolute;
    top: -1px;
    right: -1px;
    width: 0.5rem;
    height: 0.5rem;
    border-radius: 9999px;
    background: var(--color-link);
    box-shadow: 0 0 0 2px var(--color-surface);
  }

  @keyframes pop {
    from {
      opacity: 0;
      transform: scale(0.85);
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .jump {
      animation: none;
    }
  }
</style>
