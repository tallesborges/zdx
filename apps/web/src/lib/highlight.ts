/**
 * Compact syntax highlighter for the diff view.
 *
 * Deliberately not a full language parser. Diff hunks are *fragments* — a line
 * can start inside a string or block comment whose opener was never sent — so a
 * stateful lexer would mis-colour as often as it helped. This scans line-local
 * structure only and collapses everything into the six token classes the
 * theme actually defines (`--hljs-keyword`, `-string`, `-comment`, `-number`,
 * `-function`, `-class`).
 */

export type TokenType = "keyword" | "string" | "comment" | "number" | "function" | "class";

export interface Range {
  start: number;
  end: number;
}

export interface TokenRange extends Range {
  type: TokenType;
}

export interface Span {
  text: string;
  token?: TokenType;
  changed: boolean;
}

interface LangSpec {
  lineComment: string[];
  blockComment?: [string, string];
  quotes: string[];
  /** Rust/OCaml-style `'a` lifetimes must not be read as an unterminated char. */
  lifetimes?: boolean;
  keywords: Set<string>;
  builtins?: Set<string>;
}

const s = (words: string) => new Set(words.split(/\s+/).filter(Boolean));

const RUST: LangSpec = {
  lineComment: ["//"],
  blockComment: ["/*", "*/"],
  quotes: ['"'],
  lifetimes: true,
  keywords: s(`as async await break const continue crate dyn else enum extern false fn for if
    impl in let loop match mod move mut pub ref return self Self static struct super trait true
    type unsafe use where while union macro_rules`),
  builtins: s(`bool char str String Vec Option Result Some None Ok Err Box Rc Arc RefCell Cell
    HashMap HashSet BTreeMap usize isize u8 u16 u32 u64 u128 i8 i16 i32 i64 i128 f32 f64`),
};

const TS: LangSpec = {
  lineComment: ["//"],
  blockComment: ["/*", "*/"],
  quotes: ['"', "'", "`"],
  keywords: s(`abstract any as async await break case catch class const continue debugger
    declare default delete do else enum export extends false finally for from function get
    if implements import in instanceof interface keyof let new null of package private
    protected public readonly return satisfies set static super switch this throw true try
    type typeof undefined var void while with yield`),
  builtins: s(`Array Boolean Date Error JSON Map Math Number Object Promise RegExp Set String
    Symbol WeakMap WeakSet BigInt console document window globalThis`),
};

const CSS_LANG: LangSpec = {
  lineComment: [],
  blockComment: ["/*", "*/"],
  quotes: ['"', "'"],
  keywords: s(`important from to and or not only all screen print`),
  builtins: s(`var calc rgb rgba hsl hsla url light-dark color-mix clamp min max minmax repeat
    linear-gradient radial-gradient translate scale rotate cubic-bezier`),
};

const PY: LangSpec = {
  lineComment: ["#"],
  quotes: ['"', "'"],
  keywords: s(`and as assert async await break class continue def del elif else except False
    finally for from global if import in is lambda None nonlocal not or pass raise return
    True try while with yield match case`),
  builtins: s(`bool bytes dict float int list set str tuple print len range enumerate zip
    open super self Exception ValueError TypeError`),
};

const GO: LangSpec = {
  lineComment: ["//"],
  blockComment: ["/*", "*/"],
  quotes: ['"', "`"],
  keywords: s(`break case chan const continue default defer else fallthrough for func go goto
    if import interface map package range return select struct switch type var nil true false`),
  builtins: s(`bool byte complex64 complex128 error float32 float64 int int8 int16 int32 int64
    rune string uint uint8 uint16 uint32 uint64 uintptr make new len cap append copy delete panic`),
};

const SHELL: LangSpec = {
  lineComment: ["#"],
  quotes: ['"', "'"],
  keywords: s(`if then else elif fi for while until do done case esac function return in
    local export source alias set unset trap exit`),
  builtins: s(`echo cd ls cat grep sed awk cut sort uniq head tail find xargs curl git make
    mkdir rm cp mv test printf read`),
};

const TOML_LANG: LangSpec = {
  lineComment: ["#"],
  quotes: ['"', "'"],
  keywords: s(`true false`),
};

const YAML_LANG: LangSpec = {
  lineComment: ["#"],
  quotes: ['"', "'"],
  keywords: s(`true false null yes no on off`),
};

const JSON_LANG: LangSpec = {
  lineComment: [],
  quotes: ['"'],
  keywords: s(`true false null`),
};

const SQL: LangSpec = {
  lineComment: ["--"],
  blockComment: ["/*", "*/"],
  quotes: ["'", '"'],
  keywords: s(`select from where insert update delete into values set create table alter drop
    index join left right inner outer on group by order limit offset having distinct as and
    or not null primary key foreign references default unique constraint returning with`),
};

const PLAIN: LangSpec = { lineComment: [], quotes: [], keywords: new Set() };

const BY_EXT: Record<string, LangSpec> = {
  rs: RUST,
  ts: TS, tsx: TS, js: TS, jsx: TS, mjs: TS, cjs: TS, mts: TS, cts: TS,
  svelte: TS, vue: TS,
  css: CSS_LANG, scss: CSS_LANG, less: CSS_LANG,
  py: PY, pyi: PY,
  go: GO,
  sh: SHELL, bash: SHELL, zsh: SHELL, fish: SHELL,
  toml: TOML_LANG,
  yaml: YAML_LANG, yml: YAML_LANG,
  json: JSON_LANG, jsonc: JSON_LANG,
  sql: SQL,
  c: GO, h: GO, cpp: GO, hpp: GO, cc: GO, java: TS, kt: TS, swift: TS,
};

const BY_NAME: Record<string, LangSpec> = {
  justfile: SHELL,
  makefile: SHELL,
  dockerfile: SHELL,
  ".env": SHELL,
  ".gitignore": PLAIN,
};

export function detectLanguage(path: string): LangSpec {
  const file = path.split("/").pop()?.toLowerCase() ?? "";
  if (BY_NAME[file]) return BY_NAME[file];
  const ext = file.includes(".") ? file.split(".").pop()! : "";
  return BY_EXT[ext] ?? PLAIN;
}

const IDENT_START = /[A-Za-z_$]/;
const IDENT = /[A-Za-z0-9_$]/;
const DIGIT = /[0-9]/;

/** Scan one line into non-overlapping token ranges. */
export function tokenize(text: string, lang: LangSpec): TokenRange[] {
  if (lang === PLAIN) return [];

  const out: TokenRange[] = [];
  const n = text.length;
  let i = 0;

  while (i < n) {
    const ch = text[i];

    // Line comment — consumes the rest of the line.
    const lc = lang.lineComment.find((p) => text.startsWith(p, i));
    if (lc) {
      out.push({ start: i, end: n, type: "comment" });
      break;
    }

    // Block comment. An unterminated opener runs to EOL, which is the correct
    // reading for a fragment.
    if (lang.blockComment && text.startsWith(lang.blockComment[0], i)) {
      const close = text.indexOf(lang.blockComment[1], i + lang.blockComment[0].length);
      const end = close === -1 ? n : close + lang.blockComment[1].length;
      out.push({ start: i, end, type: "comment" });
      i = end;
      continue;
    }

    // Rust lifetime (`'a`) rather than a char literal.
    if (lang.lifetimes && ch === "'" && IDENT_START.test(text[i + 1] ?? "")) {
      let j = i + 1;
      while (j < n && IDENT.test(text[j])) j++;
      if (text[j] !== "'") {
        i = j;
        continue;
      }
    }

    if (lang.quotes.includes(ch)) {
      let j = i + 1;
      while (j < n) {
        if (text[j] === "\\") j += 2;
        else if (text[j] === ch) { j++; break; }
        else j++;
      }
      out.push({ start: i, end: Math.min(j, n), type: "string" });
      i = Math.min(j, n);
      continue;
    }

    if (DIGIT.test(ch) || (ch === "." && DIGIT.test(text[i + 1] ?? ""))) {
      let j = i;
      while (j < n && /[0-9a-fA-FxXoObB._]/.test(text[j])) j++;
      // Trailing type suffixes (`10u32`, `1.5f64`, `100px`).
      while (j < n && IDENT.test(text[j])) j++;
      out.push({ start: i, end: j, type: "number" });
      i = j;
      continue;
    }

    if (IDENT_START.test(ch)) {
      let j = i;
      while (j < n && IDENT.test(text[j])) j++;
      const word = text.slice(i, j);

      let k = j;
      while (k < n && text[k] === " ") k++;
      const callish = text[k] === "(";

      let type: TokenType | null = null;
      if (lang.keywords.has(word)) type = "keyword";
      else if (lang.builtins?.has(word)) type = "class";
      else if (callish) type = "function";
      else if (/^[A-Z]/.test(word) && word.length > 1) type = "class";

      if (type) out.push({ start: i, end: j, type });
      i = j;
      continue;
    }

    i++;
  }

  return out;
}

/**
 * Flatten syntax tokens and word-diff ranges into one non-overlapping span list.
 * Both range sets are independent, so we cut the line at every boundary either
 * one introduces and resolve attributes per resulting slice.
 */
export function buildSpans(text: string, tokens: TokenRange[], changed: Range[] = []): Span[] {
  if (!text) return [];
  if (tokens.length === 0 && changed.length === 0) {
    return [{ text, changed: false }];
  }

  const cuts = new Set<number>([0, text.length]);
  for (const t of tokens) {
    cuts.add(t.start);
    cuts.add(t.end);
  }
  for (const c of changed) {
    cuts.add(c.start);
    cuts.add(c.end);
  }

  const points = [...cuts].filter((p) => p >= 0 && p <= text.length).sort((a, b) => a - b);
  const spans: Span[] = [];

  for (let i = 0; i < points.length - 1; i++) {
    const start = points[i];
    const end = points[i + 1];
    if (start === end) continue;

    const token = tokens.find((t) => t.start <= start && t.end >= end)?.type;
    const isChanged = changed.some((c) => c.start <= start && c.end >= end);
    const slice = text.slice(start, end);

    const prev = spans[spans.length - 1];
    if (prev && prev.token === token && prev.changed === isChanged) {
      prev.text += slice;
    } else {
      spans.push({ text: slice, token, changed: isChanged });
    }
  }

  return spans;
}
