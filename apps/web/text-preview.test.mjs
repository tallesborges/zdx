import { expect, test } from "bun:test";
import { decodeTextPreview, splitLines } from "./src/lib/textPreview.ts";

const utf8 = (s) => new TextEncoder().encode(s);

test("decodes UTF-8 and reports no cut for a small file", () => {
  expect(decodeTextPreview(utf8("héllo\n"), 1024)).toEqual({
    binary: false,
    text: "héllo\n",
    truncated: false,
    shownBytes: 7,
  });
});

test("cuts at the limit and drops a character split by the cut", () => {
  // "é" is two bytes; a 2-byte limit keeps "a" and half of "é".
  const result = decodeTextPreview(utf8("aéz"), 2);
  expect(result).toEqual({ binary: false, text: "a", truncated: true, shownBytes: 2 });
});

test("a NUL byte marks the file as not text", () => {
  expect(decodeTextPreview(new Uint8Array([0x50, 0x4b, 0x00, 0x03]))).toEqual({ binary: true });
});

test("invalid bytes inside the text become U+FFFD", () => {
  const result = decodeTextPreview(new Uint8Array([0x61, 0xff, 0x62]));
  expect(result.binary).toBe(false);
  expect(result.text).toBe("a\uFFFDb");
});

test("splits on every newline style without a trailing empty line", () => {
  expect(splitLines("")).toEqual([]);
  expect(splitLines("a\r\nb\rc\n")).toEqual(["a", "b", "c"]);
  expect(splitLines("a\n\n")).toEqual(["a", ""]);
  expect(splitLines("\n")).toEqual([""]);
});
