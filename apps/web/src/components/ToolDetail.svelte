<script lang="ts">
  import type { ToolNode } from "$lib/transcript";
  import type { ToolFact, ToolFactKind } from "$lib/types";
  import { router } from "$lib/router.svelte";
  import { selectionChanged } from "$lib/telegram";
  import Markdown from "./Markdown.svelte";

  interface Props {
    node: ToolNode;
    /** Facts the host already shows (the trajectory sheet has its own status and timing). */
    omitFacts?: ToolFactKind[];
  }

  let { node, omitFacts = [] }: Props = $props();
  let showRaw = $state(false);
  let promptOpen = $state(false);
  let copied = $state<"" | "Copied" | "Copy failed">("");

  let view = $derived(node.view);
  let body = $derived(view?.body);
  /** Live output wins while the call runs; the view has no output yet. */
  let liveTail = $derived(node.result ? "" : (node.running?.output_tail ?? ""));
  let outside = $derived(!liveTail && (body?.kind === "markdown" || body?.kind === "error"));

  /** The view's facts; a call without a result has only what the live run or its absence says. */
  let facts = $derived.by((): ToolFact[] => {
    const run = node.running;
    if (run && !node.result) {
      if (!run.completed_at)
        return [{ text: `running ${run.running_for}`, bad: false, kind: "outcome" }];
      const failed = run.ok === false;
      return [
        { text: failed ? "failed" : "done", bad: failed, kind: "outcome" },
        ...(run.running_for
          ? [{ text: run.running_for, bad: false, kind: "duration" as const }]
          : []),
      ];
    }
    if (!node.result) return [{ text: "no result recorded", bad: false, kind: "outcome" }];
    return view?.status ?? [];
  });
  let status = $derived(facts.filter((fact) => !omitFacts.includes(fact.kind)));

  function lines(text: string): string[] {
    const out = text.split("\n");
    if (out.length > 1 && out[out.length - 1] === "") out.pop();
    return out;
  }

  /** A `<pre>` would show the final newline as an empty last row. */
  function withoutFinalNewline(text: string): string {
    return text.endsWith("\n") ? text.slice(0, -1) : text;
  }

  function splitPath(path: string): [string, string] {
    const i = path.lastIndexOf("/");
    return i < 0 ? ["", path] : [path.slice(0, i + 1), path.slice(i + 1)];
  }

  function pretty(value: unknown): string {
    if (typeof value === "string") return value;
    try {
      return JSON.stringify(value, null, 2);
    } catch {
      return String(value);
    }
  }

  let raw = $derived.by(() => {
    const parts: { label: string; text: string }[] = [];
    if (node.input !== null && node.input !== undefined)
      parts.push({ label: "Input", text: pretty(node.input) });
    if (node.result) parts.push({ label: "Output", text: pretty(node.result.output) });
    else if (node.running)
      parts.push({ label: "Output so far", text: node.running.output_tail || "(none yet)" });
    return parts;
  });

  function toggleRaw() {
    showRaw = !showRaw;
    copied = "";
    selectionChanged();
  }

  async function copyRaw() {
    const text = JSON.stringify(
      { name: node.name, input: node.input, output: node.result?.output ?? null },
      null,
      2,
    );
    try {
      await navigator.clipboard.writeText(text);
      copied = "Copied";
    } catch {
      copied = "Copy failed";
    }
  }
</script>

{#snippet head()}
  {#if view?.head}
    {@const h = view.head}
    <div class="head">
      {#if h.kind === "shell"}
        {#if h.label}<div class="label">{h.label}</div>{/if}
        {#if h.cwd}<div class="cwd">in {h.cwd}</div>{/if}
        <div class="cmd"><span class="ps">$</span> {h.command}</div>
      {:else if h.kind === "path"}
        {@const [dir, file] = splitPath(h.path)}
        <div class="path">{dir}<b>{file}</b></div>
      {:else if h.kind === "search"}
        <div class="path">
          <span class="pattern">{h.pattern}</span>{#if h.scope}<span class="dim">{" in "}</span>{h.scope}{/if}
        </div>
      {:else if h.kind === "prompt"}
        {#if h.agent}<div class="agent">{h.agent}</div>{/if}
        <button
          type="button"
          class="prompt"
          class:clamped={!promptOpen}
          aria-expanded={promptOpen}
          onclick={() => (promptOpen = !promptOpen)}>{h.prompt}</button
        >
      {:else}
        <div class="cmd">{h.text}</div>
      {/if}
    </div>
  {/if}
{/snippet}

{#snippet output()}
  {#if liveTail}
    <pre class="txt">{liveTail}</pre>
  {:else if body?.kind === "streams"}
    {#if body.streams.length === 0}
      <p class="empty">No output</p>
    {/if}
    {#each body.streams as stream, i (i)}
      {#if stream.name === "stderr" && body.streams.length > 1}
        <p class="stream">stderr</p>
      {/if}
      <pre class="txt" class:err={stream.name === "stderr"}>{withoutFinalNewline(stream.text)}</pre>
    {/each}
  {:else if body?.kind === "code"}
    {@const rows = lines(body.text)}
    <div class="code scroll">
      {#each rows as row, i (i)}
        <span class="ln">{body.first_line + i}</span><span class="src">{row || " "}</span>
      {/each}
    </div>
  {:else if body?.kind === "matches"}
    <div class="scroll">
      {#if body.files.length === 0}
        <p class="empty">No matches</p>
      {/if}
      {#each body.files as file, i (i)}
        {@const [dir, name] = splitPath(file.path)}
        {@const keepIndent = file.lines.some((l) => l.context)}
        <p class="gfile">{dir}<b>{name}</b></p>
        <div class="code">
          {#each file.lines as m, j (j)}
            <span class="ln">{m.line}</span><span class="src" class:dim={m.context}
              >{keepIndent ? m.text : m.text.trim()}</span
            >
          {/each}
        </div>
      {/each}
    </div>
  {:else if body?.kind === "paths"}
    {#if body.entries.length === 0}
      <p class="empty">Nothing found</p>
    {:else}
      <ul class="flist scroll">
        {#each body.entries as entry, i (i)}
          {@const [dir, name] = splitPath(entry.path)}
          <li><span class="dim">{dir}</span>{name}{entry.dir ? "/" : ""}</li>
        {/each}
      </ul>
    {/if}
  {:else if body?.kind === "diff"}
    <div class="diff scroll">
      {#each body.lines as line, i (i)}
        <div class={line.op}>{line.op === "add" ? "+" : line.op === "del" ? "-" : " "} {line.text}</div>
      {/each}
    </div>
  {:else if body?.kind === "text"}
    <pre class="txt">{body.text}</pre>
  {:else if node.running && !node.result}
    <p class="empty">Waiting for output…</p>
  {:else if !view}
    <p class="empty">No result recorded.</p>
  {/if}
{/snippet}

<div class="detail">
  {#if outside}
    {#if view?.head}<div class="box">{@render head()}</div>{/if}
    {#if body?.kind === "markdown"}
      <div class="answer"><Markdown source={body.text} /></div>
    {:else if body?.kind === "error"}
      <div class="errcard">
        <p class="code-id">{body.code}</p>
        {#if body.message}<p class="msg">{body.message}</p>{/if}
        {#if body.details}<pre>{body.details}</pre>{/if}
      </div>
    {/if}
  {:else}
    {@const hasOutput = liveTail || (body && body.kind !== "empty") || node.running || !view}
    {#if view?.head || hasOutput}
      <div class="box">
        {@render head()}
        {#if hasOutput}
          <div class="body" class:ruled={!!view?.head}>{@render output()}</div>
        {/if}
      </div>
    {/if}
  {/if}

  {#if view?.notes.length && !liveTail}
    <ul class="notes">
      {#each view.notes as note, i (i)}
        <li>{note}</li>
      {/each}
    </ul>
  {/if}

  {#if view?.info?.length}
    <ul class="info">
      {#each view.info as line, i (i)}
        <li>{line}</li>
      {/each}
    </ul>
  {/if}

  <div class="status">
    {#each status as fact, i (i)}
      {#if i > 0}<span aria-hidden="true">·</span>{/if}
      <span class:bad={fact.bad}>{fact.text}</span>
    {/each}
    <span class="sp"></span>
    {#if view?.thread_id}
      <button type="button" class="sbtn" onclick={() => router.openThread(view.thread_id!)}>
        Open thread
      </button>
    {/if}
    <button type="button" class="sbtn" aria-pressed={showRaw} onclick={toggleRaw}>{"{ } JSON"}</button>
  </div>

  {#if showRaw}
    <div class="raw">
      {#each raw as part, i (i)}
        <p class="rl">{part.label}</p>
        <pre class="out">{part.text}</pre>
      {/each}
      <div class="rawbar">
        <button type="button" class="sbtn" onclick={copyRaw}>Copy JSON</button>
        {#if copied}<span class="copied">{copied}</span>{/if}
      </div>
    </div>
  {/if}
</div>

<style>
  .detail {
    padding: 0.25rem 0 0.6rem;
    min-width: 0;
  }

  .box {
    border-radius: 7px;
    background: var(--color-editor-background);
    color: var(--color-editor-foreground);
    overflow: hidden;
    min-width: 0;
  }
  .box + .answer,
  .box + .errcard {
    margin-top: 0.4rem;
  }

  .head {
    padding: 0.45rem 0.6rem 0.4rem;
    font-family: var(--font-mono);
    font-size: 0.75rem;
    line-height: 1.55;
    min-width: 0;
  }
  .body.ruled {
    border-top: 1px solid var(--color-border);
  }

  .cwd {
    font-size: 0.6875rem;
    color: var(--color-muted-foreground);
    overflow-wrap: anywhere;
  }
  .label {
    color: var(--color-muted-foreground);
    font-size: 0.6875rem;
    overflow-wrap: anywhere;
  }
  .cmd {
    padding-left: 2ch;
    text-indent: -2ch;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .ps {
    color: var(--color-diff-del-fg);
  }
  .path {
    overflow-wrap: anywhere;
  }
  .path b {
    font-weight: 600;
    color: var(--color-foreground);
  }
  .pattern {
    color: var(--color-warning);
  }
  .dim {
    color: var(--color-muted-foreground);
  }
  .agent {
    font-size: 0.6875rem;
    font-weight: 600;
    color: var(--color-muted-foreground);
  }
  .prompt {
    display: block;
    width: 100%;
    text-align: left;
    font-family: var(--font-sans);
    font-size: 0.78rem;
    line-height: 1.5;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .prompt.clamped {
    display: -webkit-box;
    -webkit-line-clamp: 3;
    line-clamp: 3;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }

  .scroll,
  .txt {
    max-height: 18rem;
    overflow: auto;
    -webkit-overflow-scrolling: touch;
  }
  .txt {
    margin: 0;
    padding: 0.45rem 0.6rem 0.5rem;
    font-family: var(--font-mono);
    font-size: 0.75rem;
    line-height: 1.55;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .txt.err {
    color: var(--color-destructive);
  }
  .txt + .stream,
  .txt + .txt {
    border-top: 1px dashed var(--color-border);
  }
  .stream {
    margin: 0;
    padding: 0.45rem 0.6rem 0;
    font-size: 0.6rem;
    font-weight: 600;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--color-destructive);
  }
  .empty {
    margin: 0;
    padding: 0.45rem 0.6rem 0.5rem;
    font-size: 0.75rem;
    font-style: italic;
    color: var(--color-muted-foreground);
  }

  .code {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr);
    padding: 0.35rem 0 0.5rem;
    font-family: var(--font-mono);
    font-size: 0.75rem;
    line-height: 1.55;
  }
  .code .ln {
    padding: 0 0.6rem;
    text-align: right;
    color: var(--color-muted-foreground);
    opacity: 0.75;
    user-select: none;
  }
  .code .src {
    padding-right: 0.6rem;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .gfile {
    margin: 0;
    padding: 0.45rem 0.6rem 0;
    font-family: var(--font-mono);
    font-size: 0.7rem;
    color: var(--color-muted-foreground);
    overflow-wrap: anywhere;
  }
  .gfile b {
    font-weight: 600;
    color: var(--color-foreground);
  }
  .gfile + .code {
    padding-top: 0.1rem;
  }

  .flist {
    margin: 0;
    padding: 0.4rem 0.6rem 0.5rem;
    list-style: none;
    font-family: var(--font-mono);
    font-size: 0.75rem;
    line-height: 1.6;
  }
  .flist li {
    overflow-wrap: anywhere;
  }

  .diff {
    padding: 0.3rem 0 0.4rem;
    font-family: var(--font-mono);
    font-size: 0.75rem;
    line-height: 1.55;
  }
  .diff div {
    padding: 0 0.6rem 0 2.2ch;
    text-indent: -1.4ch;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .diff .add {
    background: var(--color-diff-add-bg);
    color: var(--color-diff-add-fg);
  }
  .diff .del {
    background: var(--color-diff-del-bg);
    color: var(--color-diff-del-fg);
  }
  .diff .ctx {
    color: var(--color-muted-foreground);
  }

  .answer {
    font-size: 0.875rem;
  }
  .answer :global(.zdx-prose) {
    font-size: 0.875rem;
  }

  .errcard {
    min-width: 0;
    padding: 0.5rem 0.6rem 0.55rem;
    border-left: 3px solid var(--color-destructive);
    border-radius: 7px;
    background: var(--color-diff-del-bg);
  }
  .errcard p {
    margin: 0;
  }
  .code-id {
    font-family: var(--font-mono);
    font-size: 0.72rem;
    font-weight: 600;
    color: var(--color-diff-del-fg);
  }
  .msg {
    margin-top: 0.2rem !important;
    font-size: 0.8125rem;
    overflow-wrap: anywhere;
  }
  .errcard pre {
    margin: 0.4rem 0 0;
    max-height: 12rem;
    overflow: auto;
    font-family: var(--font-mono);
    font-size: 0.72rem;
    line-height: 1.5;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    color: var(--color-muted-foreground);
  }

  .notes {
    margin: 0.4rem 0 0;
    padding: 0;
    list-style: none;
    display: grid;
    gap: 0.25rem;
    font-size: 0.78rem;
    line-height: 1.45;
    color: var(--color-warning);
  }
  .notes li {
    overflow-wrap: anywhere;
  }
  .notes li::before {
    content: "⚠ ";
  }

  .info {
    margin: 0.35rem 0 0;
    padding: 0;
    list-style: none;
    display: grid;
    gap: 0.2rem;
    font-family: var(--font-mono);
    font-size: 0.7rem;
    line-height: 1.45;
    color: var(--color-muted-foreground);
  }
  .info li {
    overflow-wrap: anywhere;
  }

  .status {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.3rem 0.45rem;
    margin-top: 0.4rem;
    font-family: var(--font-mono);
    font-size: 0.72rem;
    line-height: 1.4;
    color: var(--color-muted-foreground);
  }
  .status .bad {
    color: var(--color-diff-del-fg);
  }
  .status .sp {
    flex: 1;
  }
  .sbtn {
    min-height: 1.75rem;
    padding: 0 0.55rem;
    border: 1px solid var(--color-border);
    border-radius: 6px;
    font-family: var(--font-sans);
    font-size: 0.72rem;
    font-weight: 500;
    color: var(--color-muted-foreground);
  }
  .sbtn[aria-pressed="true"] {
    color: var(--color-foreground);
    border-color: var(--color-muted-foreground);
  }

  .raw {
    margin-top: 0.5rem;
    display: grid;
    gap: 0.25rem;
  }
  .rl {
    margin: 0.25rem 0 0;
    font-size: 0.625rem;
    font-weight: 500;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--color-muted-foreground);
  }
  .out {
    margin: 0;
    max-height: 16rem;
    overflow: auto;
    -webkit-overflow-scrolling: touch;
    padding: 0.5rem;
    border-radius: 6px;
    background: var(--color-editor-background);
    color: var(--color-editor-foreground);
    font-family: var(--font-mono);
    font-size: 0.6875rem;
    line-height: 1.55;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .rawbar {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    margin-top: 0.15rem;
  }
  .copied {
    font-size: 0.72rem;
    color: var(--color-muted-foreground);
  }
</style>
