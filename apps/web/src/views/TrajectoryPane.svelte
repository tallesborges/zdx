<script lang="ts">
  import { selectionChanged } from "$lib/telegram";
  import type {
    Json,
    ThreadActivity,
    ThreadTrajectoryReport,
    TrajectoryBottleneck,
    TrajectorySpan,
    TrajectoryTurn,
  } from "$lib/types";

  interface Props {
    trajectory: ThreadTrajectoryReport;
    activity: ThreadActivity[];
  }

  let { trajectory, activity }: Props = $props();

  let mode = $state<"timeline" | "bottlenecks">("timeline");
  let selectedKey = $state<string | null>(null);
  let slowest = $derived(trajectory.bottlenecks[0] ?? null);
  let selected = $derived(
    selectedKey
      ? (trajectory.turns.flatMap((turn) => turn.spans).find((span) => span.key === selectedKey) ??
          null)
      : null,
  );
  let selectedInput = $derived(selected ? spanInput(selected) : undefined);
  let selectedOutput = $derived(selected ? spanOutput(selected) : undefined);

  function select(span: TrajectorySpan) {
    selectionChanged();
    selectedKey = span.key;
  }

  function setMode(next: "timeline" | "bottlenecks") {
    selectionChanged();
    mode = next;
  }

  function closeDetail() {
    selectedKey = null;
  }

  function onKeydown(event: KeyboardEvent) {
    if (event.key === "Escape" && selectedKey) closeDetail();
  }

  function domain(turn: TrajectoryTurn): number {
    return Math.max(1, (turn.domain_end_ms ?? 0) - (turn.domain_start_ms ?? 0));
  }

  function position(turn: TrajectoryTurn, span: TrajectorySpan): string {
    const range = domain(turn);
    const left = (((span.start_ms ?? turn.domain_start_ms ?? 0) - (turn.domain_start_ms ?? 0)) / range) * 100;
    const width = (((span.end_ms ?? span.start_ms ?? 0) - (span.start_ms ?? 0)) / range) * 100;
    return `left:${Math.max(0, left)}%;width:max(${Math.max(0, width)}%,2px);top:${span.track * 2 + 0.25}rem`;
  }

  function laneHeight(tracks: number): string {
    return `height:${Math.max(2.25, tracks * 2 + 0.5)}rem`;
  }

  function axisLabel(turn: TrajectoryTurn, fraction: number): string {
    return formatTrajectoryDuration((turn.elapsed_ms ?? 0) * fraction);
  }

  function clock(value: string | undefined): string {
    if (!value) return "—";
    const parsed = new Date(value);
    if (Number.isNaN(parsed.valueOf())) return value;
    return parsed.toISOString().slice(11, 23);
  }

  function text(value: Json | string | undefined): string {
    if (value === undefined) return "";
    const rendered = typeof value === "string" ? value : JSON.stringify(value, null, 2);
    return rendered.length > 12_000 ? `${rendered.slice(0, 12_000)}\n…` : rendered;
  }

  function share(span: TrajectoryBottleneck): number {
    const max = slowest?.duration_ms ?? 1;
    return Math.max(2, (span.duration_ms / max) * 100);
  }

  function timingLabel(span: Pick<TrajectorySpan, "status" | "timing">): string {
    if (span.status === "running") return "live span";
    if (span.timing === "exact") return "exact span";
    if (span.timing === "duration") return "duration only";
    return "unavailable";
  }

  function timedSpans(turn: TrajectoryTurn): TrajectorySpan[] {
    return turn.spans.filter((span) => span.kind !== "input" && span.timing === "exact");
  }

  function unanchoredSpans(turn: TrajectoryTurn): TrajectorySpan[] {
    return turn.spans.filter((span) => span.kind !== "input" && span.timing !== "exact");
  }

  function spanDuration(span: TrajectorySpan): number | undefined {
    return span.duration_ms ?? span.wall_ms;
  }

  function selectBottleneck(bottleneck: TrajectoryBottleneck) {
    const span = trajectory.turns
      .flatMap((turn) => turn.spans)
      .find((candidate) => candidate.key === bottleneck.key);
    if (span) select(span);
  }

  function spanInput(span: TrajectorySpan): Json | string | undefined {
    if (span.live_input !== undefined) return span.live_input;
    const source = activity.find((item) => item.sequence === span.sequence);
    if (source?.type === "message") return source.text;
    if (source?.type === "tool_use") return source.input;
    return undefined;
  }

  function spanOutput(span: TrajectorySpan): Json | string | undefined {
    if (span.live_output_tail !== undefined) return span.live_output_tail;
    if (span.result_sequence === undefined) return undefined;
    const result = activity.find((item) => item.sequence === span.result_sequence);
    return result?.type === "tool_result" ? result.output : undefined;
  }

  function formatTrajectoryDuration(ms: number | undefined): string {
    if (ms === undefined) return "—";
    if (ms < 1_000) return `${Math.round(ms)}ms`;
    if (ms < 10_000) return `${(ms / 1_000).toFixed(1)}s`;
    if (ms < 60_000) return `${Math.round(ms / 1_000)}s`;
    const minutes = Math.floor(ms / 60_000);
    const seconds = Math.round((ms % 60_000) / 1_000);
    return seconds ? `${minutes}m ${seconds}s` : `${minutes}m`;
  }
</script>

<svelte:window onkeydown={onKeydown} />

<div class="trajectory scroll-area flex-1">
  <div class="summary">
    <div class="summary-copy">
      <p class="eyebrow">Trajectory</p>
      {#if slowest}
        <h2>
          <span>Slowest</span>
          {slowest.label}
          <strong>{formatTrajectoryDuration(slowest.duration_ms)}</strong>
        </h2>
        <p>
          Turn {slowest.turn} · {slowest.detail} · {timingLabel(slowest)}
        </p>
      {:else}
        <h2>No measured spans yet</h2>
        <p>New activity will appear here as model requests and tools finish.</p>
      {/if}
    </div>

    <div class="metrics" aria-label="Trajectory summary">
      <div>
        <span>Active wall</span>
        <strong>{formatTrajectoryDuration(trajectory.active_wall_ms)}</strong>
      </div>
      <div>
        <span>Span work</span>
        <strong>{formatTrajectoryDuration(trajectory.span_work_ms)}</strong>
      </div>
      <div>
        <span>Concurrent</span>
        <strong>{formatTrajectoryDuration(trajectory.concurrent_work_ms)}</strong>
      </div>
    </div>

    <p class="coverage">
      {trajectory.measured}/{trajectory.total} durations measured · {trajectory.exact} with exact boundaries
    </p>
  </div>

  <div class="view-switch" aria-label="Trajectory view">
    <button
      type="button"
      class:active={mode === "timeline"}
      aria-pressed={mode === "timeline"}
      onclick={() => setMode("timeline")}>Timeline</button
    >
    <button
      type="button"
      class:active={mode === "bottlenecks"}
      aria-pressed={mode === "bottlenecks"}
      onclick={() => setMode("bottlenecks")}>Bottlenecks</button
    >
  </div>

  {#if trajectory.turns.length === 0}
    <p class="empty">No user turns found.</p>
  {:else if mode === "timeline"}
    <div class="turns">
      {#each trajectory.turns as turn (turn.index)}
        <section class="turn">
          <header class="turn-head">
            <div>
              <span class="turn-number">Turn {turn.index}</span>
              <span class:legacy={turn.timing === "legacy"} class="quality">{turn.timing}</span>
            </div>
            <span class="turn-duration">
              {turn.elapsed_ms !== undefined ? `${formatTrajectoryDuration(turn.elapsed_ms)} elapsed` : `${turn.measured}/${turn.total} measured`}
            </span>
          </header>

          {#if turn.input}
            <p class="prompt">{turn.input.text}</p>
          {/if}

          {#if timedSpans(turn).length > 0}
            <div class="timeline-scroll">
              <div class="timeline-canvas">
                <div class="axis-row">
                  <span class="lane-label">Time</span>
                  <div class="axis">
                    {#each [0, 0.25, 0.5, 0.75, 1] as fraction (fraction)}
                      <span style={`left:${fraction * 100}%`}>{axisLabel(turn, fraction)}</span>
                    {/each}
                  </div>
                </div>

                <div class="lane-row">
                  <span class="lane-label">Input</span>
                  <div class="lane input-lane">
                    {#each turn.spans.filter((span) => span.kind === "input" && span.start_ms !== undefined) as span (span.key)}
                      <button
                        type="button"
                        class="input-point"
                        style={`left:${(((span.start_ms ?? 0) - (turn.domain_start_ms ?? 0)) / domain(turn)) * 100}%`}
                        aria-label={`Open input details for turn ${turn.index}`}
                        onclick={() => select(span)}
                      ></button>
                    {/each}
                  </div>
                </div>

                <div class="lane-row">
                  <span class="lane-label">Model</span>
                  <div class="lane" style={laneHeight(turn.model_tracks)}>
                    {#each timedSpans(turn).filter((span) => span.lane === "model") as span (span.key)}
                      <button
                        type="button"
                        class="span model"
                        class:running={span.status === "running"}
                        style={position(turn, span)}
                        aria-label={`${span.label}, ${formatTrajectoryDuration(spanDuration(span))}`}
                        onclick={() => select(span)}
                      >
                        <span>{span.label}</span>
                      </button>
                    {/each}
                  </div>
                </div>

                <div class="lane-row">
                  <span class="lane-label">Tools</span>
                  <div class="lane" style={laneHeight(turn.tool_tracks)}>
                    {#each timedSpans(turn).filter((span) => span.lane === "tools") as span (span.key)}
                      <button
                        type="button"
                        class="span tool"
                        class:failed={span.status === "failed"}
                        class:running={span.status === "running"}
                        style={position(turn, span)}
                        aria-label={`${span.label}, ${formatTrajectoryDuration(spanDuration(span))}`}
                        onclick={() => select(span)}
                      >
                        <span>{span.label}</span>
                      </button>
                    {/each}
                  </div>
                </div>
              </div>
            </div>
          {/if}

          {#if unanchoredSpans(turn).length > 0}
            <div class="unanchored">
              <p>
                {timedSpans(turn).length > 0 ? "Outside exact timeline" : "Exact overlap unavailable"}
                <span>Older or incomplete events are never positioned from inferred timestamps.</span>
              </p>
              {#each unanchoredSpans(turn) as span (span.key)}
                <button type="button" onclick={() => select(span)}>
                  <span class="kind">{span.kind === "model" ? "Model" : span.label}</span>
                  <span class="subject">{span.detail}</span>
                  <strong>{formatTrajectoryDuration(spanDuration(span))}</strong>
                </button>
              {/each}
            </div>
          {/if}

          {#if timedSpans(turn).length > 0}
            <footer class="turn-stats">
              <span>wall {formatTrajectoryDuration(turn.active_wall_ms)}</span>
              <span>work {formatTrajectoryDuration(turn.span_work_ms)}</span>
              <span>concurrent {formatTrajectoryDuration(turn.concurrent_work_ms)}</span>
            </footer>
          {/if}
        </section>
      {/each}
    </div>
  {:else}
    <section class="bottlenecks">
      <header>
        <p class="eyebrow">Ranked by recorded duration</p>
        <h3>Where the thread waited</h3>
        <p>Duration-only legacy spans can rank here, but never appear as positioned overlap.</p>
      </header>

      {#if trajectory.bottlenecks.length === 0}
        <p class="empty">No measured model or tool durations.</p>
      {:else}
        <div class="ranking">
          {#each trajectory.bottlenecks as span, index (span.key)}
            <button
              type="button"
              class:slowest={index === 0}
              class:failed={span.status === "failed"}
              onclick={() => selectBottleneck(span)}
            >
              <span class="rank">{String(index + 1).padStart(2, "0")}</span>
              <span class="rank-main">
                <span class="rank-title">
                  <strong>{span.label}</strong>
                  <small>Turn {span.turn} · {timingLabel(span)}</small>
                  <b>{formatTrajectoryDuration(span.duration_ms)}</b>
                </span>
                <span class="rank-detail">{span.detail}</span>
                <span class="bar"><i style={`width:${share(span)}%`}></i></span>
              </span>
            </button>
          {/each}
        </div>
      {/if}

      {#if trajectory.total > trajectory.measured}
        <p class="unmeasured">
          {trajectory.total - trajectory.measured} span{trajectory.total - trajectory.measured === 1 ? "" : "s"}
          excluded because no duration was recorded.
        </p>
      {/if}
    </section>
  {/if}
</div>

{#if selected}
  <div class="detail-layer">
    <button class="detail-scrim" type="button" aria-label="Close span details" onclick={closeDetail}></button>
    <dialog class="detail-sheet" open aria-label="Span details">
      <header>
        <div>
          <p class="eyebrow">Turn {selected.turn} · {selected.kind}</p>
          <h3>{selected.label}</h3>
          <p>{selected.detail}</p>
        </div>
        <button type="button" aria-label="Close" onclick={closeDetail}>×</button>
      </header>

      <div class="detail-metrics">
        <div><span>Duration</span><strong>{formatTrajectoryDuration(spanDuration(selected))}</strong></div>
        <div><span>Timing</span><strong>{timingLabel(selected)}</strong></div>
        <div><span>Started</span><strong>{clock(selected.started_at)}</strong></div>
        <div><span>Completed</span><strong>{clock(selected.completed_at)}</strong></div>
        {#if selected.ttft_ms !== undefined}
          <div><span>TTFT</span><strong>{formatTrajectoryDuration(selected.ttft_ms)}</strong></div>
        {/if}
        <div><span>Status</span><strong class:failed-text={selected.status === "failed"}>{selected.status}</strong></div>
      </div>

      {#if selected.kind === "model"}
        <div class="model-meta">
          <span>{selected.provider ?? "unknown provider"}</span>
          <span>{selected.model ?? "unknown model"}</span>
          <span>{(selected.input_tokens ?? 0).toLocaleString()} in</span>
          <span>{(selected.output_tokens ?? 0).toLocaleString()} out</span>
          <span>{(selected.cache_read_tokens ?? 0).toLocaleString()} cached</span>
        </div>
      {/if}

      {#if selectedInput !== undefined && text(selectedInput)}
        <div class="payload">
          <h4>{selected.kind === "input" ? "Message" : "Payload"}</h4>
          <pre>{text(selectedInput)}</pre>
        </div>
      {/if}

      {#if selectedOutput !== undefined && text(selectedOutput)}
        <div class="payload">
          <h4>{selected.status === "running" ? "Output tail" : "Result"}</h4>
          <pre>{text(selectedOutput)}</pre>
        </div>
      {/if}
    </dialog>
  </div>
{/if}

<style>
  .trajectory {
    padding: 0.75rem 0.625rem calc(var(--tg-safe-bottom) + 1rem);
    background:
      linear-gradient(180deg, color-mix(in srgb, var(--color-warning) 5%, transparent), transparent 9rem),
      var(--color-surface);
  }

  .summary {
    position: relative;
    overflow: hidden;
    padding: 1rem;
    border: 1px solid var(--color-border);
    border-radius: 0.75rem;
    background: color-mix(in srgb, var(--color-card) 72%, var(--color-surface));
  }

  .summary::after {
    position: absolute;
    top: -2.5rem;
    right: -2.5rem;
    width: 8rem;
    height: 8rem;
    border: 1px solid color-mix(in srgb, var(--color-warning) 24%, transparent);
    border-radius: 50%;
    content: "";
  }

  .eyebrow {
    margin: 0;
    font-family: var(--font-mono);
    font-size: 0.625rem;
    font-weight: 600;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--color-muted-foreground);
  }

  .summary h2 {
    position: relative;
    z-index: 1;
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 0.25rem 0.45rem;
    margin: 0.25rem 0 0;
    font-size: 1.05rem;
    line-height: 1.25;
    letter-spacing: -0.025em;
  }

  .summary h2 span {
    font-weight: 500;
    color: var(--color-muted-foreground);
  }

  .summary h2 strong {
    font-family: var(--font-mono);
    font-size: 0.9rem;
    color: var(--color-warning);
  }

  .summary-copy > p:last-child,
  .bottlenecks > header > p:last-child {
    margin: 0.3rem 0 0;
    font-size: 0.6875rem;
    color: var(--color-muted-foreground);
  }

  .metrics {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 1px;
    overflow: hidden;
    margin-top: 0.875rem;
    border: 1px solid var(--color-border);
    border-radius: 0.5rem;
    background: var(--color-border);
  }

  .metrics > div {
    min-width: 0;
    padding: 0.55rem 0.45rem;
    background: var(--color-surface);
  }

  .metrics span,
  .detail-metrics span {
    display: block;
    font-size: 0.5625rem;
    line-height: 1.2;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--color-muted-foreground);
  }

  .metrics strong {
    display: block;
    overflow: hidden;
    margin-top: 0.15rem;
    font-family: var(--font-mono);
    font-size: 0.75rem;
    text-overflow: ellipsis;
  }

  .coverage {
    margin: 0.6rem 0 0;
    font-family: var(--font-mono);
    font-size: 0.6rem;
    color: var(--color-muted-foreground);
  }

  .view-switch {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 0.2rem;
    margin: 0.75rem 0;
    padding: 0.2rem;
    border-radius: 0.55rem;
    background: var(--color-muted);
  }

  .view-switch button {
    min-height: 2.15rem;
    border-radius: 0.4rem;
    font-size: 0.75rem;
    font-weight: 500;
    color: var(--color-muted-foreground);
  }

  .view-switch button.active {
    background: var(--color-surface);
    box-shadow: 0 1px 2px light-dark(#00000012, #00000055);
    color: var(--color-foreground);
  }

  .turns {
    display: grid;
    gap: 0.625rem;
  }

  .turn {
    overflow: hidden;
    border: 1px solid var(--color-border);
    border-radius: 0.65rem;
    background: color-mix(in srgb, var(--color-card) 45%, var(--color-surface));
  }

  .turn-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.5rem;
    padding: 0.65rem 0.75rem 0.35rem;
  }

  .turn-number {
    font-size: 0.75rem;
    font-weight: 650;
  }

  .quality {
    margin-left: 0.4rem;
    padding: 0.12rem 0.35rem;
    border-radius: 999px;
    background: color-mix(in srgb, var(--color-success) 12%, transparent);
    font-family: var(--font-mono);
    font-size: 0.5625rem;
    text-transform: uppercase;
    color: var(--color-success);
  }

  .quality.legacy {
    background: var(--color-muted);
    color: var(--color-muted-foreground);
  }

  .turn-duration {
    font-family: var(--font-mono);
    font-size: 0.625rem;
    color: var(--color-muted-foreground);
  }

  .prompt {
    display: -webkit-box;
    overflow: hidden;
    margin: 0;
    padding: 0 0.75rem 0.65rem;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    font-size: 0.6875rem;
    color: var(--color-muted-foreground);
  }

  .timeline-scroll {
    overflow-x: auto;
    border-block: 1px solid var(--color-border);
    background: color-mix(in srgb, var(--color-editor-background) 45%, var(--color-surface));
    scrollbar-width: thin;
    -webkit-overflow-scrolling: touch;
  }

  .timeline-canvas {
    min-width: 35rem;
    padding-block: 0.45rem 0.6rem;
  }

  .axis-row,
  .lane-row {
    display: grid;
    grid-template-columns: 3.5rem minmax(0, 1fr);
    align-items: stretch;
  }

  .lane-label {
    position: sticky;
    left: 0;
    z-index: 4;
    display: flex;
    align-items: center;
    padding-left: 0.65rem;
    border-right: 1px solid var(--color-border);
    background: color-mix(in srgb, var(--color-editor-background) 75%, var(--color-surface));
    font-family: var(--font-mono);
    font-size: 0.5625rem;
    letter-spacing: 0.03em;
    text-transform: uppercase;
    color: var(--color-muted-foreground);
  }

  .axis {
    position: relative;
    height: 1.25rem;
    margin-inline: 0.65rem;
    border-bottom: 1px solid var(--color-border);
  }

  .axis span {
    position: absolute;
    bottom: 0.18rem;
    transform: translateX(-50%);
    font-family: var(--font-mono);
    font-size: 0.5rem;
    color: var(--color-muted-foreground);
  }

  .axis span:first-child { transform: none; }
  .axis span:last-child { transform: translateX(-100%); }

  .lane {
    position: relative;
    min-height: 2.25rem;
    margin-inline: 0.65rem;
    background-image: linear-gradient(to right, var(--color-border) 1px, transparent 1px);
    background-size: 25% 100%;
  }

  .input-lane { height: 2rem; min-height: 2rem; }

  .input-point {
    position: absolute;
    top: 0.55rem;
    width: 0.75rem;
    height: 0.75rem;
    transform: translateX(-50%) rotate(45deg);
    border: 2px solid var(--color-surface);
    border-radius: 2px;
    background: var(--color-foreground);
    box-shadow: 0 0 0 1px var(--color-border);
  }

  .input-point::after,
  .span::after {
    position: absolute;
    inset: -0.45rem;
    content: "";
  }

  .span {
    position: absolute;
    z-index: 1;
    overflow: hidden;
    height: 1.5rem;
    min-width: 2px;
    border-radius: 0.3rem;
    text-align: left;
    box-shadow: inset 0 0 0 1px light-dark(#00000014, #ffffff12);
  }

  .span > span {
    display: block;
    overflow: hidden;
    padding: 0.25rem 0.4rem;
    font-family: var(--font-mono);
    font-size: 0.5625rem;
    line-height: 1rem;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .span.model {
    background: color-mix(in srgb, var(--color-link) 22%, var(--color-surface));
    color: var(--color-link);
  }

  .span.tool {
    background: color-mix(in srgb, var(--color-warning) 22%, var(--color-surface));
    color: var(--color-warning);
  }

  .span.failed {
    background: color-mix(in srgb, var(--color-destructive) 20%, var(--color-surface));
    color: var(--color-destructive);
  }

  .span.running {
    background-image: repeating-linear-gradient(
      135deg,
      color-mix(in srgb, var(--color-warning) 24%, var(--color-surface)) 0 0.35rem,
      color-mix(in srgb, var(--color-warning) 10%, var(--color-surface)) 0.35rem 0.7rem
    );
    animation: pulse 1.4s ease-in-out infinite;
  }

  .unanchored {
    padding: 0.65rem 0.75rem;
  }

  .unanchored > p {
    margin: 0 0 0.35rem;
    font-size: 0.6875rem;
    font-weight: 600;
  }

  .unanchored > p span {
    display: block;
    margin-top: 0.1rem;
    font-weight: 400;
    color: var(--color-muted-foreground);
  }

  .unanchored button {
    display: grid;
    width: 100%;
    min-height: 2.35rem;
    grid-template-columns: 4.25rem minmax(0, 1fr) auto;
    align-items: center;
    gap: 0.45rem;
    border-top: 1px solid var(--color-border);
    text-align: left;
  }

  .unanchored .kind,
  .unanchored strong {
    font-family: var(--font-mono);
    font-size: 0.625rem;
  }

  .unanchored .kind { color: var(--color-warning); }
  .unanchored .subject {
    overflow: hidden;
    font-size: 0.6875rem;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--color-muted-foreground);
  }

  .turn-stats {
    display: flex;
    gap: 0.75rem;
    padding: 0.45rem 0.75rem;
    border-top: 1px solid var(--color-border);
    font-family: var(--font-mono);
    font-size: 0.5625rem;
    color: var(--color-muted-foreground);
  }

  .bottlenecks {
    padding: 0.25rem 0 1rem;
  }

  .bottlenecks h3 {
    margin: 0.2rem 0 0;
    font-size: 1rem;
    letter-spacing: -0.02em;
  }

  .ranking {
    margin-top: 0.75rem;
    border-block: 1px solid var(--color-border);
  }

  .ranking > button {
    display: grid;
    width: 100%;
    grid-template-columns: 2rem minmax(0, 1fr);
    gap: 0.5rem;
    padding: 0.7rem 0.25rem;
    border-bottom: 1px solid var(--color-border);
    text-align: left;
  }

  .ranking > button:last-child { border-bottom: 0; }
  .ranking > button.slowest { background: color-mix(in srgb, var(--color-warning) 7%, transparent); }

  .rank {
    padding-top: 0.05rem;
    font-family: var(--font-mono);
    font-size: 0.625rem;
    color: var(--color-muted-foreground);
  }

  .rank-title {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr) auto;
    align-items: baseline;
    gap: 0.4rem;
  }

  .rank-title strong { font-size: 0.75rem; }
  .rank-title small {
    overflow: hidden;
    font-family: var(--font-mono);
    font-size: 0.5625rem;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--color-muted-foreground);
  }
  .rank-title b {
    font-family: var(--font-mono);
    font-size: 0.6875rem;
    color: var(--color-warning);
  }
  .ranking > button.failed .rank-title b { color: var(--color-destructive); }

  .rank-detail {
    display: block;
    overflow: hidden;
    margin-top: 0.1rem;
    font-size: 0.6875rem;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--color-muted-foreground);
  }

  .bar {
    display: block;
    height: 2px;
    margin-top: 0.45rem;
    background: var(--color-muted);
  }

  .bar i {
    display: block;
    height: 100%;
    background: var(--color-warning);
  }

  .unmeasured,
  .empty {
    margin: 0.75rem 0;
    font-size: 0.6875rem;
    color: var(--color-muted-foreground);
  }

  .detail-layer {
    position: fixed;
    z-index: 50;
    inset: 0;
    display: flex;
    align-items: flex-end;
    justify-content: center;
  }

  .detail-scrim {
    position: absolute;
    inset: 0;
    width: 100%;
    background: #0008;
  }

  .detail-sheet {
    position: relative;
    width: min(100%, 42rem);
    max-height: min(82dvh, 46rem);
    overflow-y: auto;
    padding: 0.9rem 0.9rem calc(var(--tg-safe-bottom) + 1rem);
    border: 1px solid var(--color-border);
    border-bottom: 0;
    border-radius: 0.9rem 0.9rem 0 0;
    background: var(--color-surface);
    box-shadow: 0 -1rem 3rem #0004;
    animation: sheet-in 180ms cubic-bezier(0.2, 0.8, 0.2, 1);
  }

  .detail-sheet > header {
    display: flex;
    align-items: flex-start;
    gap: 0.75rem;
  }

  .detail-sheet > header > div { min-width: 0; flex: 1; }
  .detail-sheet h3 { margin: 0.15rem 0 0; font-size: 1rem; }
  .detail-sheet > header p:last-child {
    overflow: hidden;
    margin: 0.15rem 0 0;
    font-size: 0.6875rem;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--color-muted-foreground);
  }

  .detail-sheet > header > button {
    width: 2rem;
    height: 2rem;
    border-radius: 50%;
    background: var(--color-muted);
    font-size: 1.25rem;
    line-height: 1;
    color: var(--color-muted-foreground);
  }

  .detail-metrics {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 0.45rem;
    margin-top: 0.85rem;
  }

  .detail-metrics > div {
    min-width: 0;
    padding: 0.55rem;
    border-radius: 0.45rem;
    background: var(--color-card);
  }

  .detail-metrics strong {
    display: block;
    overflow: hidden;
    margin-top: 0.15rem;
    font-family: var(--font-mono);
    font-size: 0.6875rem;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .failed-text { color: var(--color-destructive); }

  .model-meta {
    display: flex;
    flex-wrap: wrap;
    gap: 0.3rem;
    margin-top: 0.75rem;
  }

  .model-meta span {
    padding: 0.2rem 0.4rem;
    border-radius: 999px;
    background: var(--color-muted);
    font-family: var(--font-mono);
    font-size: 0.5625rem;
    color: var(--color-muted-foreground);
  }

  .payload { margin-top: 0.85rem; }
  .payload h4 {
    margin: 0 0 0.35rem;
    font-size: 0.6875rem;
    text-transform: uppercase;
    color: var(--color-muted-foreground);
  }
  .payload pre {
    overflow: auto;
    max-height: 18rem;
    margin: 0;
    padding: 0.7rem;
    border-radius: 0.5rem;
    background: var(--color-editor-background);
    color: var(--color-editor-foreground);
    font-family: var(--font-mono);
    font-size: 0.625rem;
    line-height: 1.5;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }

  @keyframes pulse {
    50% { opacity: 0.62; }
  }

  @keyframes sheet-in {
    from { transform: translateY(100%); }
  }

  @media (min-width: 48rem) {
    .trajectory { padding-inline: 1rem; }
    .detail-metrics { grid-template-columns: repeat(3, minmax(0, 1fr)); }
  }

  @media (prefers-reduced-motion: reduce) {
    .span.running,
    .detail-sheet { animation: none; }
  }
</style>