<script lang="ts">
  import { renderMarkdown } from "$lib/markdown";

  interface Props {
    source: string;
  }

  let { source }: Props = $props();
  let html = $derived(renderMarkdown(source));
</script>

<div class="zdx-prose">
  {@html html}
</div>

<style>
  /* Prose sizing tuned for a phone-width Telegram WebView: tight leading,
     generous block spacing, code that never forces a horizontal page scroll. */
  .zdx-prose {
    font-size: 0.9375rem;
    line-height: 1.6;
    color: var(--color-foreground);
    overflow-wrap: anywhere;
  }

  .zdx-prose :global(> *:first-child) {
    margin-top: 0;
  }
  .zdx-prose :global(> *:last-child) {
    margin-bottom: 0;
  }

  .zdx-prose :global(p),
  .zdx-prose :global(ul),
  .zdx-prose :global(ol),
  .zdx-prose :global(blockquote) {
    margin: 0.6em 0;
  }

  .zdx-prose :global(h1),
  .zdx-prose :global(h2),
  .zdx-prose :global(h3),
  .zdx-prose :global(h4) {
    margin: 1.2em 0 0.5em;
    font-weight: 600;
    line-height: 1.25;
    letter-spacing: -0.01em;
  }

  .zdx-prose :global(h1) { font-size: 1.25rem; }
  .zdx-prose :global(h2) { font-size: 1.125rem; }
  .zdx-prose :global(h3) { font-size: 1rem; }

  .zdx-prose :global(ul),
  .zdx-prose :global(ol) {
    padding-left: 1.25em;
  }
  .zdx-prose :global(ul) { list-style: disc; }
  .zdx-prose :global(ol) { list-style: decimal; }
  .zdx-prose :global(li) { margin: 0.25em 0; }

  .zdx-prose :global(a) {
    color: var(--color-link);
    text-decoration: underline;
    text-underline-offset: 2px;
  }

  .zdx-prose :global(blockquote) {
    border-left: 2px solid var(--color-border);
    padding-left: 0.75em;
    color: var(--color-muted-foreground);
  }

  .zdx-prose :global(code) {
    font-family: var(--font-mono);
    font-size: 0.8125em;
    background: var(--color-accent);
    padding: 0.1em 0.3em;
    border-radius: 4px;
  }

  .zdx-prose :global(pre) {
    margin: 0.75em 0;
    padding: 0.75em;
    border: 1px solid var(--color-border);
    border-radius: 8px;
    background: var(--color-editor-background);
    color: var(--color-editor-foreground);
    overflow-x: auto;
    -webkit-overflow-scrolling: touch;
  }

  .zdx-prose :global(pre code) {
    background: none;
    padding: 0;
    font-size: 0.75rem;
    line-height: 1.6;
  }

  .zdx-prose :global(hr) {
    border: 0;
    border-top: 1px solid var(--color-border);
    margin: 1.25em 0;
  }

  .zdx-prose :global(table) {
    display: block;
    width: max-content;
    max-width: 100%;
    overflow-x: auto;
    border-collapse: collapse;
    font-size: 0.8125rem;
  }
  .zdx-prose :global(th),
  .zdx-prose :global(td) {
    border: 1px solid var(--color-border);
    padding: 0.35em 0.6em;
    text-align: left;
  }
  .zdx-prose :global(th) {
    background: var(--color-accent);
    font-weight: 600;
  }
</style>
