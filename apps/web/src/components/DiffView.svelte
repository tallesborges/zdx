<script lang="ts">
  import { parseDiff, diffStats } from "$lib/diff";
  import { buildSpans, detectLanguage, tokenize } from "$lib/highlight";
  import type { Span } from "$lib/highlight";

  interface Props {
    content: string;
    path?: string;
    truncated?: boolean;
    limitBytes?: number;
  }

  let { content, path = "", truncated = false, limitBytes = 0 }: Props = $props();

  let lines = $derived(parseDiff(content));
  let stats = $derived(diffStats(lines));
  let lang = $derived(detectLanguage(path));

  // Very large diffs are rare but real (the API caps at 256 KiB); skip
  // highlighting rather than tokenizing thousands of lines on the main thread.
  let highlightable = $derived(lines.length <= 3000);

  let rendered = $derived(
    lines.map((line): Span[] => {
      if (line.type === "hunk" || line.type === "meta") {
        return [{ text: line.text, changed: false }];
      }
      const tokens = highlightable ? tokenize(line.text, lang) : [];
      return buildSpans(line.text, tokens, line.changed ?? []);
    }),
  );

  let gutterWidth = $derived(
    Math.max(2, String(Math.max(...lines.map((l) => l.newNo ?? l.oldNo ?? 0), 0)).length),
  );
</script>

<div class="overflow-hidden rounded-lg border border-border">
  <div
    class="flex items-center gap-3 border-b border-border bg-accent px-2.5 py-1.5 font-mono text-xxs"
  >
    <span class="text-diff-add-fg">+{stats.additions}</span>
    <span class="text-diff-del-fg">−{stats.deletions}</span>
    {#if truncated}
      <span class="ml-auto text-warning"
        >truncated at {(limitBytes / 1024).toFixed(0)} KiB</span
      >
    {/if}
  </div>

  {#if lines.length === 0}
    <p class="px-2.5 py-3 text-xs text-muted-foreground">
      No textual diff — the file may be empty or binary.
    </p>
  {:else}
    <div class="diff-scroll">
      <table class="diff" style="--gutter: {gutterWidth}ch">
        <tbody>
          {#each lines as line, i (i)}
            <tr class="row {line.type}">
              <td class="no">{line.oldNo ?? ""}</td>
              <td class="no">{line.newNo ?? ""}</td>
              <td class="mark"
                >{line.type === "add" ? "+" : line.type === "del" ? "−" : ""}</td
              >
              <td class="code">
                {#each rendered[i] as span, s (s)}
                  <span class={span.token ?? ""} class:changed={span.changed}>{span.text}</span>
                {/each}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</div>

<style>
  .diff-scroll {
    overflow-x: auto;
    -webkit-overflow-scrolling: touch;
    background: var(--color-background);
  }

  .diff {
    border-collapse: collapse;
    width: max-content;
    min-width: 100%;
    font-family: var(--font-mono);
    font-size: 0.75rem;
    line-height: 1.5;
    tab-size: 2;
  }

  .row.add {
    background: var(--color-diff-add-bg);
  }
  .row.del {
    background: var(--color-diff-del-bg);
  }
  .row.hunk {
    background: var(--color-accent);
  }
  .row.hunk .code,
  .row.meta .code {
    color: var(--color-diff-meta-fg);
  }

  .no {
    /* Line numbers stay pinned while long lines scroll horizontally. */
    position: sticky;
    left: 0;
    width: var(--gutter);
    min-width: var(--gutter);
    padding: 0 0.4em;
    text-align: right;
    color: var(--color-muted-foreground);
    background: inherit;
    user-select: none;
    font-variant-numeric: tabular-nums;
    opacity: 0.65;
  }
  .no:nth-child(2) {
    left: calc(var(--gutter) + 0.8em);
    box-shadow: 1px 0 0 0 var(--color-border);
  }

  .mark {
    width: 1.2em;
    padding-left: 0.4em;
    text-align: center;
    user-select: none;
  }
  .row.add .mark {
    color: var(--color-diff-add-fg);
  }
  .row.del .mark {
    color: var(--color-diff-del-fg);
  }

  .code {
    padding: 0 0.75em 0 0.3em;
    white-space: pre;
    color: var(--color-foreground);
  }

  /* The six syntax classes. */
  .code :global(.keyword) { color: var(--hljs-keyword); }
  .code :global(.string) { color: var(--hljs-string); }
  .code :global(.comment) { color: var(--hljs-comment); font-style: italic; }
  .code :global(.number) { color: var(--hljs-number); }
  .code :global(.function) { color: var(--hljs-function); }
  .code :global(.class) { color: var(--hljs-class); }

  /* Diff chrome outranks syntax colour. */
  .row.hunk .code :global(span),
  .row.meta .code :global(span) {
    color: inherit;
  }

  /* Intra-line treatment: tint the changed words, keep the token colour. */
  .code :global(.changed) {
    border-radius: 2px;
    padding-block: 0.1667em;
  }
  .row.add .code :global(.changed) {
    background: color-mix(in srgb, var(--color-diff-add-bg) 55%, var(--color-diff-add-fg));
  }
  .row.del .code :global(.changed) {
    background: color-mix(in srgb, var(--color-diff-del-bg) 55%, var(--color-diff-del-fg));
  }
</style>
