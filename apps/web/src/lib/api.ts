import { tg } from "./telegram";
import type {
  ArtifactsResponse,
  GitDiffResponse,
  GitFileKind,
  GitResponse,
  GitScopeResponse,
  MonitorResponse,
  ThreadListResponse,
  ThreadResponse,
  ThreadTrajectoryReport,
  WorkersResponse,
} from "./types";

/**
 * All `/api/*` routes require `Authorization: tma <initData>` signed by the bot
 * token and belonging to an allowlisted Telegram user. Errors come back as
 * plain text, not JSON.
 */
export class ApiError extends Error {
  constructor(
    message: string,
    readonly status: number,
  ) {
    super(message);
    this.name = "ApiError";
  }

  /** initData is only valid for an hour — the fix is reopening the Mini App. */
  get isAuth(): boolean {
    return this.status === 401 || this.status === 403;
  }
}

/**
 * Dev-only escape hatch: outside Telegram there is no signed initData, so paste
 * a real one into `.env.local` as ZDX_DEV_INIT_DATA to work against a live bot.
 */
const devInitData = import.meta.env.ZDX_DEV_INIT_DATA as string | undefined;

function authHeader(): string | null {
  const initData = tg?.initData || devInitData;
  return initData ? `tma ${initData}` : null;
}

async function get<T>(path: string, params: Record<string, string> = {}): Promise<T> {
  const auth = authHeader();
  if (!auth) {
    throw new ApiError("Open this from Telegram to authenticate.", 401);
  }

  const url = new URL(path, window.location.origin);
  for (const [k, v] of Object.entries(params)) url.searchParams.set(k, v);
  // The server sets no cache headers; make sure WebViews never serve a stale body.
  url.searchParams.set("t", String(Date.now()));

  const res = await fetch(url, {
    headers: { Authorization: auth },
    cache: "no-store",
  });

  if (!res.ok) {
    const body = (await res.text().catch(() => "")).trim();
    if (res.status === 401 || res.status === 403) {
      throw new ApiError("Telegram authorization expired. Close and reopen the Mini App.", res.status);
    }
    throw new ApiError(body || `Request failed (${res.status})`, res.status);
  }

  return (await res.json()) as T;
}

async function getBlob(path: string, params: Record<string, string> = {}): Promise<Blob> {
  const auth = authHeader();
  if (!auth) {
    throw new ApiError("Open this from Telegram to authenticate.", 401);
  }

  const url = new URL(path, window.location.origin);
  for (const [k, v] of Object.entries(params)) url.searchParams.set(k, v);
  url.searchParams.set("t", String(Date.now()));

  const res = await fetch(url, {
    headers: { Authorization: auth },
    cache: "no-store",
  });

  if (!res.ok) {
    const body = (await res.text().catch(() => "")).trim();
    if (res.status === 401 || res.status === 403) {
      throw new ApiError("Telegram authorization expired. Close and reopen the Mini App.", res.status);
    }
    throw new ApiError(body || `Request failed (${res.status})`, res.status);
  }

  return await res.blob();
}

export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return "0 B";
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit++;
  }
  return `${value >= 100 ? Math.round(value) : value.toFixed(1)} ${units[unit]}`;
}

export const api = {
  threads: async () => {
    if (import.meta.env.DEV) {
      const demo = await import("./demo");
      if (demo.demoEnabled()) return demo.demoThreads;
    }
    return get<ThreadListResponse>("/api/threads");
  },

  thread: async (id: string, after?: number) => {
    if (import.meta.env.DEV) {
      const demo = await import("./demo");
      if (demo.demoEnabled()) return demo.demoThread;
    }
    const params = after === undefined ? undefined : { after: String(after) };
    return get<ThreadResponse>(`/api/threads/${encodeURIComponent(id)}`, params);
  },

  trajectory: async (id: string) => {
    if (import.meta.env.DEV) {
      const demo = await import("./demo");
      if (demo.demoEnabled()) return demo.demoTrajectory;
    }
    return get<ThreadTrajectoryReport>(`/api/threads/${encodeURIComponent(id)}/trajectory`);
  },

  workers: async (id: string) => {
    if (import.meta.env.DEV) {
      const demo = await import("./demo");
      if (demo.demoEnabled()) return demo.demoWorkers;
    }
    return get<WorkersResponse>(`/api/threads/${encodeURIComponent(id)}/workers`);
  },

  monitor: async () => {
    if (import.meta.env.DEV) {
      const demo = await import("./demo");
      if (demo.demoEnabled()) return demo.demoMonitor;
    }
    return get<MonitorResponse>("/api/monitor");
  },

  git: async (threadId: string) => {
    if (import.meta.env.DEV) {
      const demo = await import("./demo");
      if (demo.demoEnabled()) return demo.demoGit;
    }
    return get<GitResponse>("/api/git", { thread_id: threadId });
  },

  gitDiff: async (threadId: string, kind: GitFileKind, path: string, commit?: string) => {
    if (import.meta.env.DEV) {
      const demo = await import("./demo");
      if (demo.demoEnabled()) return { ...demo.demoDiff, path, kind };
    }
    const params: Record<string, string> = { thread_id: threadId, kind, path };
    if (commit) params.commit = commit;
    return get<GitDiffResponse>("/api/git/diff", params);
  },

  gitScope: async (threadId: string, scope: string) => {
    if (import.meta.env.DEV) {
      const demo = await import("./demo");
      if (demo.demoEnabled()) return demo.demoScope;
    }
    return get<GitScopeResponse>("/api/git/scope", { thread_id: threadId, scope });
  },

  artifacts: async (id: string) => {
    if (import.meta.env.DEV) {
      const demo = await import("./demo");
      if (demo.demoEnabled()) return demo.demoArtifacts;
    }
    return get<ArtifactsResponse>(`/api/threads/${encodeURIComponent(id)}/artifacts`);
  },

  artifactBlob: async (id: string, path: string) => {
    if (import.meta.env.DEV) {
      const demo = await import("./demo");
      if (demo.demoEnabled()) return demo.demoArtifactBlob(path);
    }
    return getBlob(`/api/threads/${encodeURIComponent(id)}/artifacts/file`, { path });
  },
};
