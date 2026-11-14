/**
 * `Open artifact` deep links from Telegram replies.
 *
 * The bot (`ArtifactLink` in `apps/gateway/internal/render/keyboards.go`)
 * publishes `startapp=a_<key>_<thread>`: `key` is the artifact's `key` in the
 * shared `zdx_engine::core::artifacts` projection (16 lowercase hex; it does
 * not embed the raw path) and `thread` is the chat or topic that received the
 * reply. Any other value is a bare thread id, as before. The key is an opaque
 * lookup id, not an authorization: it is only looked up in the thread's own
 * artifact list, fetched with the normal authenticated request.
 *
 * No `$lib` imports, so `artifact-link.test.mjs` can load it under `bun test`.
 */
import type { ArtifactItem } from "./types";

const KEY = /^[0-9a-f]{16}$/;
const ARTIFACT_START = /^a_([0-9a-f]{16})_([A-Za-z0-9_-]+)$/;

export interface StartTarget {
  id: string;
  /** Artifact key, or `""` for a plain thread link. */
  artifact: string;
}

/** The thread (and artifact) a `startapp` value names; `null` when empty. */
export function parseStartParam(raw: string): StartTarget | null {
  const value = raw.trim();
  if (!value) return null;
  const match = ARTIFACT_START.exec(value);
  // Never `active`: it names no fixed thread, and the preview can send feedback.
  if (match && match[2] !== "active") return { id: match[2], artifact: match[1] };
  return { id: value, artifact: "" };
}

export function isArtifactKey(value: string): boolean {
  return KEY.test(value);
}

/** The listed artifact a link names, matched on the projection's `key`. */
export function findLinkedArtifact(items: ArtifactItem[], key: string): ArtifactItem | undefined {
  if (!isArtifactKey(key)) return undefined;
  return items.find((item) => item.key === key);
}
