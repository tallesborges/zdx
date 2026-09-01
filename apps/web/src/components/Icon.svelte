<script lang="ts" module>
  /**
   * Inline Lucide icons (ISC). Vendored as raw markup rather than pulled in as a
   * dependency: we need a handful, and the whole app is one small bundle.
   *
   * Stroke width is 1.75 rather than Lucide's default 2 — at 14px the default
   * reads too heavy next to 13px text.
   */
  export type IconName =
    | "bot"
    | "message-square"
    | "git-branch"
    | "activity"
    | "arrow-down";

  const PATHS: Record<IconName, string> = {
    bot: `<path d="M12 8V4H8"/><rect width="16" height="12" x="4" y="8" rx="2"/><path d="M2 14h2"/><path d="M20 14h2"/><path d="M15 13v2"/><path d="M9 13v2"/>`,
    "message-square": `<path d="M22 17a2 2 0 0 1-2 2H6.828a2 2 0 0 0-1.414.586l-2.202 2.202A.71.71 0 0 1 2 21.286V5a2 2 0 0 1 2-2h16a2 2 0 0 1 2 2z"/>`,
    "git-branch": `<path d="M15 6a9 9 0 0 0-9 9V3"/><circle cx="18" cy="6" r="3"/><circle cx="6" cy="18" r="3"/>`,
    activity: `<path d="M22 12h-2.48a2 2 0 0 0-1.93 1.46l-2.35 8.36a.25.25 0 0 1-.48 0L9.24 2.18a.25.25 0 0 0-.48 0l-2.35 8.36A2 2 0 0 1 4.49 12H2"/>`,
    "arrow-down": `<path d="M12 5v14"/><path d="m19 12-7 7-7-7"/>`,
  };

  export const ICON_PATHS = PATHS;
</script>

<script lang="ts">
  interface Props {
    name: IconName;
    size?: string;
  }

  let { name, size = "0.875rem" }: Props = $props();
</script>

<svg
  viewBox="0 0 24 24"
  fill="none"
  stroke="currentColor"
  stroke-width="1.75"
  stroke-linecap="round"
  stroke-linejoin="round"
  style="width: {size}; height: {size}"
  aria-hidden="true"
>
  {@html ICON_PATHS[name]}
</svg>

<style>
  svg {
    flex: none;
  }
</style>
