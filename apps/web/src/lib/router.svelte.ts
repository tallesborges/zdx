/**
 * Query-string router.
 *
 * Canonical shapes:
 *   ?view=thread&id=<id>&tab=agent|transcript|changes
 *   ?view=monitor&section=overview|agents|services|usage|config|automations
 *
 * The bot already publishes `?view=threads&id=…`, `?view=git&id=…` and bare
 * `startapp=<thread_id>` deep links, so those are accepted and normalized rather
 * than broken.
 */
import { startParam } from "./telegram";

export type View = "thread" | "monitor";
export type ThreadTab = "agent" | "transcript" | "changes";
export type MonitorSection =
  | "overview"
  | "agents"
  | "services"
  | "usage"
  | "config"
  | "automations";

export interface Route {
  view: View;
  id: string;
  tab: ThreadTab;
  section: MonitorSection;
}

const THREAD_TABS: ThreadTab[] = ["agent", "transcript", "changes"];
const MONITOR_SECTIONS: MonitorSection[] = [
  "overview",
  "agents",
  "services",
  "usage",
  "config",
  "automations",
];

export const THREAD_TAB_LABELS: Record<ThreadTab, string> = {
  agent: "Agent",
  transcript: "Thread",
  changes: "Changes",
};

export const MONITOR_SECTION_LABELS: Record<MonitorSection, string> = {
  overview: "Overview",
  agents: "Active agents",
  services: "Services",
  usage: "Usage",
  config: "Config",
  automations: "Automations",
};

function readRoute(): Route {
  const params = new URLSearchParams(window.location.search);
  const id = params.get("id")?.trim() || "";
  const rawView = params.get("view")?.trim() ?? "";
  const rawTab = params.get("tab")?.trim() as ThreadTab | undefined;
  const rawSection = params.get("section")?.trim() as MonitorSection | undefined;

  const tab = rawTab && THREAD_TABS.includes(rawTab) ? rawTab : "transcript";
  const section =
    rawSection && MONITOR_SECTIONS.includes(rawSection) ? rawSection : "overview";

  // Legacy link shapes published by the bot.
  if (rawView === "git") return { view: "thread", id: id || "active", tab: "changes", section };
  if (rawView === "threads") {
    return { view: "thread", id: id || "active", tab, section };
  }

  if (rawView === "thread") return { view: "thread", id: id || "active", tab, section };
  if (rawView === "monitor") return { view: "monitor", id: id || "active", tab, section };
  if (id) return { view: "thread", id, tab, section };

  // Cold open from a Telegram deep link: startapp carries the thread id.
  const start = startParam();
  if (start) return { view: "thread", id: start, tab: "transcript", section };

  return { view: "monitor", id: "active", tab, section };
}

class Router {
  current = $state<Route>(readRoute());

  constructor() {
    window.addEventListener("popstate", () => {
      this.current = readRoute();
    });
    this.replace(this.current);
  }

  private url(route: Route): string {
    const params = new URLSearchParams({ view: route.view });
    if (route.view === "thread") {
      params.set("id", route.id);
      params.set("tab", route.tab);
    } else {
      params.set("section", route.section);
    }
    // Keep the dev fixture flag across navigation.
    if (new URLSearchParams(window.location.search).has("demo")) params.set("demo", "1");
    return `${window.location.pathname}?${params}`;
  }

  private push(route: Route): void {
    this.current = route;
    // State must stay null: route is read back from the URL, and a $state proxy
    // is not structured-cloneable.
    window.history.pushState(null, "", this.url(route));
  }

  openThread(id: string, tab: ThreadTab = "transcript"): void {
    this.push({ ...this.current, view: "thread", id, tab });
  }

  setTab(tab: ThreadTab): void {
    this.push({ ...this.current, view: "thread", tab });
  }

  openMonitor(section: MonitorSection = "overview"): void {
    this.push({ ...this.current, view: "monitor", section });
  }

  replace(route: Route): void {
    window.history.replaceState(null, "", this.url(route));
  }
}

export const router = new Router();
