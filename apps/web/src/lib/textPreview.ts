/**
 * Text artifact preview: how much of a Markdown, plain-text, or code file the
 * preview shows, and how its bytes become text. No `$lib` imports, so
 * `text-preview.test.mjs` can load it under `bun test`.
 */

/** Files above this size are not fetched; the preview offers Download. */
export const TEXT_FETCH_LIMIT = 10 * 1024 * 1024;
/** Bytes rendered; the rest of a larger file is cut with a notice. */
export const TEXT_RENDER_LIMIT = 256 * 1024;
/** Above this many lines code is shown without syntax colours. */
export const HIGHLIGHT_LINE_LIMIT = 3000;

export type TextPreview =
  | { binary: true }
  | { binary: false; text: string; truncated: boolean; shownBytes: number };

/**
 * Decodes the first `limit` bytes as UTF-8. Pass at least `limit + 1` bytes
 * when the file is larger, so the cut is reported. A NUL byte means the file
 * is not text. A multi-byte character cut by the limit is dropped, not
 * replaced, and other invalid bytes become U+FFFD so a mostly-text file still
 * shows.
 */
export function decodeTextPreview(bytes: Uint8Array, limit = TEXT_RENDER_LIMIT): TextPreview {
  const shown = bytes.subarray(0, limit);
  if (shown.includes(0)) return { binary: true };
  // `stream: true` keeps an incomplete trailing sequence buffered instead of
  // emitting U+FFFD for it; it is never flushed.
  const text = new TextDecoder("utf-8").decode(shown, { stream: true });
  return { binary: false, text, truncated: bytes.length > limit, shownBytes: shown.length };
}

/** Splits text into display lines; a final newline does not add an empty line. */
export function splitLines(text: string): string[] {
  if (text === "") return [];
  const lines = text.split(/\r\n|\r|\n/);
  if (lines.length > 1 && lines[lines.length - 1] === "") lines.pop();
  return lines;
}
