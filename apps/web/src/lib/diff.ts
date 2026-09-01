/**
 * Unified-diff parser.
 *
 * `/api/git/diff` returns raw `git diff` text, so we parse it into rows rather
 * than diffing documents client-side. Paired removal/addition runs inside a hunk
 * additionally get word-level segments, which is what makes a diff readable at
 * phone width — the whole line lights up otherwise.
 */

import type { Range } from "./highlight";

export type DiffLineType = "add" | "del" | "context" | "hunk" | "meta";

export interface DiffLine {
  type: DiffLineType;
  text: string;
  oldNo: number | null;
  newNo: number | null;
  /** Character ranges differing from the paired line, for word-level marking. */
  changed?: Range[];
}

const HUNK = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/;

export function parseDiff(raw: string): DiffLine[] {
  const out: DiffLine[] = [];
  let oldNo = 0;
  let newNo = 0;

  for (const text of raw.split("\n")) {
    const hunk = HUNK.exec(text);
    if (hunk) {
      oldNo = Number(hunk[1]);
      newNo = Number(hunk[2]);
      out.push({ type: "hunk", text, oldNo: null, newNo: null });
      continue;
    }

    // Everything before the first hunk (diff --git, index, ---, +++) is metadata.
    if (
      text.startsWith("diff ") ||
      text.startsWith("index ") ||
      text.startsWith("--- ") ||
      text.startsWith("+++ ") ||
      text.startsWith("new file") ||
      text.startsWith("deleted file") ||
      text.startsWith("similarity index") ||
      text.startsWith("rename ") ||
      text.startsWith("old mode") ||
      text.startsWith("new mode") ||
      text.startsWith("Binary files")
    ) {
      out.push({ type: "meta", text, oldNo: null, newNo: null });
      continue;
    }

    if (text.startsWith("+")) {
      out.push({ type: "add", text: text.slice(1), oldNo: null, newNo: newNo++ });
    } else if (text.startsWith("-")) {
      out.push({ type: "del", text: text.slice(1), oldNo: oldNo++, newNo: null });
    } else if (text.startsWith("\\")) {
      out.push({ type: "meta", text, oldNo: null, newNo: null });
    } else {
      const body = text.startsWith(" ") ? text.slice(1) : text;
      out.push({ type: "context", text: body, oldNo: oldNo++, newNo: newNo++ });
    }
  }

  // Trailing newline in the diff body produces a bogus empty final row.
  if (out.length && out[out.length - 1].text === "" && out[out.length - 1].type === "context") {
    out.pop();
  }

  annotateWordDiff(out);
  return out;
}

/**
 * For each balanced run of removals immediately followed by additions, pair the
 * lines up and mark the differing words on both sides.
 */
function annotateWordDiff(lines: DiffLine[]): void {
  let i = 0;
  while (i < lines.length) {
    if (lines[i].type !== "del") {
      i++;
      continue;
    }

    let delEnd = i;
    while (delEnd < lines.length && lines[delEnd].type === "del") delEnd++;

    let addEnd = delEnd;
    while (addEnd < lines.length && lines[addEnd].type === "add") addEnd++;

    const dels = delEnd - i;
    const adds = addEnd - delEnd;

    if (dels > 0 && dels === adds) {
      for (let k = 0; k < dels; k++) {
        const before = lines[i + k];
        const after = lines[delEnd + k];
        const [a, b] = diffWords(before.text, after.text);
        before.changed = a;
        after.changed = b;
      }
    }

    i = addEnd > i ? addEnd : i + 1;
  }
}

const WORD = /(\s+|\w+|[^\s\w]+)/g;

interface WordToken {
  text: string;
  start: number;
}

function tokenizeWords(line: string): WordToken[] {
  const out: WordToken[] = [];
  let m: RegExpExecArray | null;
  WORD.lastIndex = 0;
  while ((m = WORD.exec(line)) !== null) {
    out.push({ text: m[0], start: m.index });
  }
  return out;
}

/** Common prefix/suffix trim — cheap and good enough for single-line pairs. */
function diffWords(before: string, after: string): [Range[], Range[]] {
  const a = tokenizeWords(before);
  const b = tokenizeWords(after);

  let start = 0;
  while (start < a.length && start < b.length && a[start].text === b[start].text) start++;

  let endA = a.length;
  let endB = b.length;
  while (endA > start && endB > start && a[endA - 1].text === b[endB - 1].text) {
    endA--;
    endB--;
  }

  // Nothing in common — marking every word adds noise, so mark the whole line.
  if (start === 0 && endA === a.length && endB === b.length) {
    return [
      before ? [{ start: 0, end: before.length }] : [],
      after ? [{ start: 0, end: after.length }] : [],
    ];
  }

  const range = (tokens: WordToken[], from: number, to: number, text: string): Range[] => {
    if (from >= to) return [];
    const last = tokens[to - 1];
    return [{ start: tokens[from].start, end: Math.min(last.start + last.text.length, text.length) }];
  };

  return [range(a, start, endA, before), range(b, start, endB, after)];
}

export interface DiffStats {
  additions: number;
  deletions: number;
}

export function diffStats(lines: DiffLine[]): DiffStats {
  let additions = 0;
  let deletions = 0;
  for (const line of lines) {
    if (line.type === "add") additions++;
    else if (line.type === "del") deletions++;
  }
  return { additions, deletions };
}
