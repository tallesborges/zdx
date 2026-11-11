<script lang="ts">
  import type { ConfigSource, WorkspaceReport, WorkspaceSetting } from "$lib/types";
  import { selectionChanged } from "$lib/telegram";

  interface Props {
    workspaces: WorkspaceReport[];
  }

  let { workspaces }: Props = $props();

  let selectedRoot = $state<string | null>(null);

  let selected = $derived(
    workspaces.find((w) => w.root === selectedRoot) ??
      workspaces.find((w) => w.current) ??
      workspaces[0] ??
      null,
  );

  // Settings grouped in server order; groups keep first-seen order.
  let groups = $derived.by(() => {
    const out: { name: string; settings: WorkspaceSetting[] }[] = [];
    for (const setting of selected?.settings ?? []) {
      const last = out[out.length - 1];
      if (last && last.name === setting.group) last.settings.push(setting);
      else out.push({ name: setting.group, settings: [setting] });
    }
    return out;
  });

  let overrideCount = $derived(
    selected?.settings.filter((s) => s.source.layer === "workspace").length ?? 0,
  );

  function pick(root: string) {
    selectionChanged();
    selectedRoot = root;
  }

  // A mode value names the tier and what it runs here now; the shared layer
  // reports `resolves_to`, null when the mode is not configured.
  function settingValueText(s: WorkspaceSetting): string {
    if (s.value === null) return "(default)";
    if (!s.value.trim().toLowerCase().startsWith("mode:")) return s.value;
    return s.resolves_to ? `${s.value} → ${s.resolves_to}` : `${s.value} (mode not configured)`;
  }

  function sourceText(source: ConfigSource): string {
    switch (source.layer) {
      case "workspace":
        return "set here";
      case "ancestor":
        return `from ${source.label}`;
      default:
        return source.label;
    }
  }
</script>

<section>
  <h2 class="sec">Workspace</h2>
  <div class="-mx-3 flex gap-1.5 overflow-x-auto px-3 pb-1">
    {#each workspaces as ws, wi (wi)}
      <button
        type="button"
        onclick={() => pick(ws.root)}
        data-active={selected?.root === ws.root}
        class="shrink-0 rounded-md border border-border bg-card px-2.5 py-1 font-mono text-xs data-[active=true]:border-foreground data-[active=true]:font-medium"
      >
        {ws.name}
      </button>
    {/each}
  </div>
</section>

{#if selected}
  <section class="rounded-md border border-border bg-card px-2.5 py-2 font-mono text-xxs">
    <p class="m-0 break-all text-muted-foreground">{selected.root}</p>
    <p class="m-0 mt-1">
      {#if selected.profiles.length}
        bound to {selected.profiles.join(", ")}
      {:else}
        no Telegram binding
      {/if}
      {#if selected.current}· bot root{/if}
    </p>
    <p class="m-0 mt-1 text-muted-foreground">
      {#if selected.has_overlay}
        {overrideCount} override{overrideCount === 1 ? "" : "s"} in .zdx/config.toml
      {:else}
        no .zdx/config.toml · everything inherited
      {/if}
    </p>
  </section>

  {#if selected.error}
    <p class="rounded-md border border-destructive px-3 py-2 text-xs text-destructive">
      {selected.error}
    </p>
  {/if}

  {#each groups as group, gi (gi)}
    <section>
      <h2 class="sec">{group.name}</h2>
      <div class="flex flex-col gap-1">
        {#each group.settings as s, si (si)}
          <div
            class="rounded-md border bg-card px-2.5 py-1.5 {s.source.layer === 'workspace'
              ? 'border-warning'
              : 'border-border'}"
          >
            <div class="flex items-baseline gap-2">
              <span class="font-mono text-xs">{s.label}</span>
              <span
                class="ml-auto shrink-0 font-mono text-xxs {s.source.layer === 'workspace'
                  ? 'text-warning'
                  : 'text-muted-foreground'}">{sourceText(s.source)}</span
              >
            </div>
            <p class="m-0 font-mono text-xxs break-all">{settingValueText(s)}</p>
            {#if s.source.layer === "workspace"}
              <p class="m-0 font-mono text-xxs text-muted-foreground break-all">
                reset → {s.inherited_value ?? "(default)"} · {sourceText(s.inherited_source)}
              </p>
            {/if}
          </div>
        {/each}
      </div>
    </section>
  {/each}

  <p class="m-0 text-xxs text-muted-foreground">
    Read-only. Edit in <span class="font-mono">zdx monitor</span> → Config:
    <span class="font-mono">p</span> workspace, <span class="font-mono">w</span> global/workspace target,
    <span class="font-mono">d</span> reset.
  </p>
{/if}

<style>
  .sec {
    margin: 0 0 0.4rem;
    font-family: var(--font-mono);
    font-size: 0.625rem;
    font-weight: 500;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--color-muted-foreground);
  }
</style>
