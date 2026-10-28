<script lang="ts">
  import { buildSpans, detectLanguage, isPlainLanguage, tokenize } from "$lib/highlight";
  import { HIGHLIGHT_LINE_LIMIT, splitLines } from "$lib/textPreview";

  interface Props {
    text: string;
    /** File name or path; picks the syntax rules. */
    path: string;
  }

  let { text, path }: Props = $props();

  let lang = $derived(detectLanguage(path));
  let lines = $derived(splitLines(text));
  // Plain text reads best wrapped on a phone; code keeps its layout until
  // the reader flips the toggle.
  let wrapChoice = $state<boolean | null>(null);
  let wrap = $derived(wrapChoice ?? isPlainLanguage(lang));
  let highlightable = $derived(lines.length <= HIGHLIGHT_LINE_LIMIT);
  let rendered = $derived(
    lines.map((line) => buildSpans(line, highlightable ? tokenize(line, lang) : [])),
  );
  let gutterWidth = $derived(Math.max(2, String(lines.length).length));
</script>

<div class="text-view">
  <div class="toolbar">
    <span>{lines.length} {lines.length === 1 ? "line" : "lines"}</span>
    <label class="wrap-toggle">
      <input type="checkbox" checked={wrap} onchange={(e) => (wrapChoice = e.currentTarget.checked)} />
      <span>Wrap</span>
    </label>
  </div>
  {#if lines.length === 0}
    <p class="empty">This file is empty.</p>
  {:else}
    <div class="scroll">
      <table class="lines" class:wrap style="--gutter: {gutterWidth}ch">
        <tbody>
          {#each rendered as spans, i (i)}
            <tr>
              <td class="no">{i + 1}</td>
              <td class="code">{#each spans as span, s (s)}<span class={span.token ?? ""}>{span.text}</span>{/each}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</div>

<style>
  .text-view {
    display: flex;
    flex-direction: column;
    min-height: 0;
  }

  .toolbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.75rem;
    border-bottom: 1px solid var(--color-border);
    padding: 0.375rem 0.75rem;
    font-size: 0.6875rem;
    color: var(--color-muted-foreground);
  }

  .wrap-toggle {
    display: inline-flex;
    align-items: center;
    gap: 0.375rem;
  }

  .empty {
    margin: 0;
    padding: 2rem 0.75rem;
    text-align: center;
    font-size: 0.75rem;
    color: var(--color-muted-foreground);
  }

  .scroll {
    overflow-x: auto;
    -webkit-overflow-scrolling: touch;
    background: var(--color-background);
  }

  .lines {
    border-collapse: collapse;
    width: max-content;
    min-width: 100%;
    font-family: var(--font-mono);
    font-size: 0.75rem;
    line-height: 1.5;
    tab-size: 4;
  }

  .lines.wrap {
    width: 100%;
    table-layout: fixed;
  }

  .no {
    /* Line numbers stay pinned while long lines scroll horizontally. */
    position: sticky;
    left: 0;
    width: calc(var(--gutter) + 0.8em);
    padding: 0 0.4em;
    vertical-align: top;
    text-align: right;
    color: var(--color-muted-foreground);
    background: var(--color-background);
    box-shadow: 1px 0 0 0 var(--color-border);
    user-select: none;
    font-variant-numeric: tabular-nums;
  }

  .code {
    padding: 0 0.75em 0 0.6em;
    white-space: pre;
    color: var(--color-foreground);
  }

  .wrap .code {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }

  /* The six syntax classes. */
  .code :global(.keyword) { color: var(--hljs-keyword); }
  .code :global(.string) { color: var(--hljs-string); }
  .code :global(.comment) { color: var(--hljs-comment); font-style: italic; }
  .code :global(.number) { color: var(--hljs-number); }
  .code :global(.function) { color: var(--hljs-function); }
  .code :global(.class) { color: var(--hljs-class); }
</style>
