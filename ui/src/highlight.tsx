import { memo } from 'react';
import Markdown, { type Components } from 'react-markdown';
import remarkGfm from 'remark-gfm';
import rehypeRaw from 'rehype-raw';
import rehypeSanitize, { defaultSchema } from 'rehype-sanitize';
import {
  useSyncExternalStore,
  type CSSProperties,
  type ReactNode,
} from 'react';
import type { HighlighterCore } from 'shiki/core';
import { findColors, type ColorHit } from './colors';

export type Lang =
  | 'ts'
  | 'sql'
  | 'prisma'
  | 'yaml'
  | 'css'
  | 'rust'
  | 'python'
  | 'go'
  | 'swift'
  | 'kotlin'
  | 'java'
  | 'c'
  | 'cpp'
  | 'objc'
  | 'php'
  | 'ruby'
  | 'json'
  | 'markdown'
  | 'shell'
  | 'toml'
  | 'dockerfile'
  | 'html'
  | 'scss'
  | 'graphql'
  | 'xml'
  | 'lua'
  | 'csharp'
  | 'dart'
  | 'vue'
  | 'svelte'
  | 'plain';

/** Extension → language, lower-cased, without the dot. `.h`/`.hpp`/`.cc`/
 *  `.mm` are grouped under `cpp` (plain C and Objective-C get their own
 *  extensions); `.tl` is explicitly plain, not a guess. */
const EXT_LANG: Record<string, Lang> = {
  ts: 'ts',
  tsx: 'ts',
  mts: 'ts',
  cts: 'ts',
  js: 'ts',
  jsx: 'ts',
  mjs: 'ts',
  cjs: 'ts',
  sql: 'sql',
  prisma: 'prisma',
  yml: 'yaml',
  yaml: 'yaml',
  css: 'css',
  rs: 'rust',
  py: 'python',
  pyw: 'python',
  pyi: 'python',
  go: 'go',
  swift: 'swift',
  kt: 'kotlin',
  kts: 'kotlin',
  java: 'java',
  c: 'c',
  h: 'cpp',
  hpp: 'cpp',
  hh: 'cpp',
  hxx: 'cpp',
  cc: 'cpp',
  cpp: 'cpp',
  cxx: 'cpp',
  mm: 'cpp',
  m: 'objc',
  php: 'php',
  rb: 'ruby',
  json: 'json',
  jsonc: 'json',
  md: 'markdown',
  markdown: 'markdown',
  mdx: 'markdown',
  sh: 'shell',
  bash: 'shell',
  zsh: 'shell',
  toml: 'toml',
  html: 'html',
  htm: 'html',
  scss: 'scss',
  graphql: 'graphql',
  gql: 'graphql',
  xml: 'xml',
  lua: 'lua',
  cs: 'csharp',
  dart: 'dart',
  vue: 'vue',
  svelte: 'svelte',
  tl: 'plain',
};

/** Best-effort language from a filename/extension; falls back to plain text. */
export function langOf(path: string): Lang {
  const base = path.slice(path.lastIndexOf('/') + 1);
  if (/^dockerfile(\..+)?$/i.test(base)) return 'dockerfile';
  const ext = base.slice(base.lastIndexOf('.') + 1).toLowerCase();
  return EXT_LANG[ext] ?? 'plain';
}

const CURLY_KW =
  /^(import|export|from|package|namespace|using|include|const|let|var|val|async|await|function|fn|func|def|return|if|else|elif|for|foreach|while|do|switch|case|default|break|continue|throw|try|catch|finally|new|delete|class|struct|interface|trait|impl|extends|implements|public|private|protected|internal|static|readonly|final|abstract|virtual|override|sealed|type|typedef|enum|void|null|nil|undefined|true|false|this|self|super|pub|mod|use|match|where|template|typename|unsigned|signed)$/;

/** Fallback keyword regex per language, used only while Shiki is still
 *  loading (or hasn't learned that grammar yet). Approximate on purpose —
 *  real highlighting takes over once the grammar loads. */
const KW: Partial<Record<Lang, RegExp>> = {
  ts: /^(import|from|export|const|let|async|await|function|return|if|for|of|in|throw|new|class|extends|public|type|enum|void|true|false|null|undefined|this)$/,
  sql: /^(CREATE|TABLE|TYPE|AS|ENUM|NOT|NULL|REFERENCES|ON|DELETE|CASCADE|PRIMARY|KEY|ALTER|ADD|COLUMN|DEFAULT|INDEX|TEXT|INTEGER)$/,
  prisma: /^(model|enum|String|Int|DateTime|Boolean)$/,
  css: /^(display|grid|flex|color|background|border|padding|margin|important)$/,
  scss: /^(display|grid|flex|color|background|border|padding|margin|important|mixin|include|extend|if|else)$/,
  rust: CURLY_KW,
  go: CURLY_KW,
  swift: CURLY_KW,
  kotlin: CURLY_KW,
  java: CURLY_KW,
  c: CURLY_KW,
  cpp: CURLY_KW,
  objc: CURLY_KW,
  php: CURLY_KW,
  csharp: CURLY_KW,
  dart: CURLY_KW,
  html: /^$/,
  xml: /^$/,
  vue: CURLY_KW,
  svelte: CURLY_KW,
  python:
    /^(def|class|import|from|as|return|if|elif|else|for|while|try|except|finally|with|lambda|yield|async|await|pass|break|continue|global|nonlocal|assert|raise|is|not|in|and|or|None|True|False|self)$/,
  ruby: /^(def|end|class|module|if|elsif|else|unless|while|until|begin|rescue|ensure|yield|return|self|nil|true|false|require|attr_accessor|puts|do)$/,
  shell:
    /^(if|then|else|elif|fi|for|while|do|done|case|esac|function|local|export|return|echo|exit|in)$/,
  toml: /^(true|false)$/,
  dockerfile:
    /^(FROM|RUN|CMD|ENTRYPOINT|COPY|ADD|WORKDIR|ENV|ARG|EXPOSE|VOLUME|USER|LABEL|ONBUILD|SHELL|STOPSIGNAL|HEALTHCHECK)$/,
  graphql:
    /^(query|mutation|subscription|type|input|enum|interface|union|schema|fragment|on|scalar|implements|extend|directive)$/,
  lua: /^(function|end|local|if|then|else|elseif|for|while|do|return|nil|true|false|and|or|not|break|in)$/,
  markdown: /^$/,
  json: /^(true|false|null)$/,
};
const NO_KW = /^$/;

const TOKEN =
  /(\/\/.*$|--.*$)|('(?:[^'\\]|\\.)*'|"(?:[^"\\]|\\.)*"|`(?:[^`\\]|\\.)*`)|(@@?\w+)|(\b\d[\d_.]*\b)|([A-Za-z_$][\w$]*)/g;

/** A highlighted piece of a line: text, CSS color, italic. */
export interface Tok {
  s: string;
  c?: string;
  i?: boolean;
}

/** Instant fallback while Shiki loads: a few regexes. */
function regexTokens(src: string, lang: Lang): Tok[] {
  if (lang === 'plain') return [{ s: src }];
  const out: Tok[] = [];
  let last = 0;
  for (const m of src.matchAll(TOKEN)) {
    const i = m.index ?? 0;
    if (i > last) out.push({ s: src.slice(last, i) });
    const [tok, comment, str, deco, num, word] = m;
    let c = '';
    if (comment && lang !== 'yaml') c = 'c';
    else if (str) c = 's';
    else if (deco && lang === 'prisma') c = 'd';
    else if (num) c = 'n';
    else if (word && (KW[lang] ?? NO_KW).test(word)) c = 'k';
    else if (word && /^[A-Z]/.test(word) && lang !== 'sql') c = 't';
    else if (word && src[i + word.length] === '(') c = 'f';
    out.push({
      s: tok ?? '',
      c: c ? `var(--tk-${c})` : undefined,
      i: c === 'c',
    });
    last = i + (tok ?? '').length;
  }
  if (last < src.length) out.push({ s: src.slice(last) });
  return out;
}

/** Shiki's own grammar id for each language. */
const SHIKI: Record<Exclude<Lang, 'plain'>, string> = {
  ts: 'typescript',
  sql: 'sql',
  prisma: 'prisma',
  yaml: 'yaml',
  css: 'css',
  rust: 'rust',
  python: 'python',
  go: 'go',
  swift: 'swift',
  kotlin: 'kotlin',
  java: 'java',
  c: 'c',
  cpp: 'cpp',
  objc: 'objective-c',
  php: 'php',
  ruby: 'ruby',
  json: 'jsonc',
  markdown: 'markdown',
  shell: 'shellscript',
  toml: 'toml',
  dockerfile: 'dockerfile',
  html: 'html',
  scss: 'scss',
  graphql: 'graphql',
  xml: 'xml',
  lua: 'lua',
  csharp: 'csharp',
  dart: 'dart',
  vue: 'vue',
  svelte: 'svelte',
};

/** One grammar loaded up front (the common web-app languages), the rest
 *  loaded lazily on first use so the eager bundle doesn't carry ~30 grammars
 *  nobody opened. Each is its own `import()`, written out literally so the
 *  bundler can split it into its own lazy chunk. */
const EAGER_LANGS: Exclude<Lang, 'plain'>[] = [
  'ts',
  'sql',
  'prisma',
  'yaml',
  'css',
];
const LANG_LOADER = {
  ts: () => import('shiki/langs/typescript.mjs'),
  sql: () => import('shiki/langs/sql.mjs'),
  prisma: () => import('shiki/langs/prisma.mjs'),
  yaml: () => import('shiki/langs/yaml.mjs'),
  css: () => import('shiki/langs/css.mjs'),
  rust: () => import('shiki/langs/rust.mjs'),
  python: () => import('shiki/langs/python.mjs'),
  go: () => import('shiki/langs/go.mjs'),
  swift: () => import('shiki/langs/swift.mjs'),
  kotlin: () => import('shiki/langs/kotlin.mjs'),
  java: () => import('shiki/langs/java.mjs'),
  c: () => import('shiki/langs/c.mjs'),
  cpp: () => import('shiki/langs/cpp.mjs'),
  objc: () => import('shiki/langs/objective-c.mjs'),
  php: () => import('shiki/langs/php.mjs'),
  ruby: () => import('shiki/langs/ruby.mjs'),
  json: () => import('shiki/langs/jsonc.mjs'),
  markdown: () => import('shiki/langs/markdown.mjs'),
  shell: () => import('shiki/langs/shellscript.mjs'),
  toml: () => import('shiki/langs/toml.mjs'),
  dockerfile: () => import('shiki/langs/dockerfile.mjs'),
  html: () => import('shiki/langs/html.mjs'),
  scss: () => import('shiki/langs/scss.mjs'),
  graphql: () => import('shiki/langs/graphql.mjs'),
  xml: () => import('shiki/langs/xml.mjs'),
  lua: () => import('shiki/langs/lua.mjs'),
  csharp: () => import('shiki/langs/csharp.mjs'),
  dart: () => import('shiki/langs/dart.mjs'),
  vue: () => import('shiki/langs/vue.mjs'),
  svelte: () => import('shiki/langs/svelte.mjs'),
} satisfies Record<Exclude<Lang, 'plain'>, () => Promise<unknown>>;

let shiki: HighlighterCore | null = null;
let readyPromise: Promise<void> | null = null;
let version = 0;
const listeners = new Set<() => void>();
/** Single lines, tokenized alone (Patch/SymbolViewer previews, and the
 *  instant fallback). Keyed by lang + the line's own text. */
const cache = new Map<string, Tok[]>();
/** Whole hunk sides, tokenized in one call so grammar state (an open
 *  comment, a template literal, …) carries across lines. Keyed by lang +
 *  the side's full text. */
const blockCache = new Map<string, Tok[][]>();
const pendingLangs = new Map<string, Promise<void>>();

function notify() {
  cache.clear();
  blockCache.clear();
  version++;
  listeners.forEach((f) => f());
}

/** Real TextMate grammars, lazily; the theme maps scopes to CSS variables
 *  built on HeroUI tokens. Only the eager set loads here — everything else
 *  loads on first use via `ensureLang`. */
export function loadShiki(): Promise<void> {
  if (readyPromise) return readyPromise;
  readyPromise = (async () => {
    const [core, js] = await Promise.all([
      import('shiki/core'),
      import('shiki/engine/javascript'),
    ]);
    shiki = await core.createHighlighterCore({
      themes: [
        core.createCssVariablesTheme({
          name: 'heroui',
          variablePrefix: '--shiki-',
          fontStyle: true,
        }),
      ],
      langs: EAGER_LANGS.map((l) => LANG_LOADER[l]()),
      engine: js.createJavaScriptRegexEngine(),
    });
    notify();
  })();
  return readyPromise;
}

export const shikiReady = () => shiki !== null;

/** Whether `lang`'s grammar is loaded and ready to tokenize with. */
function langReady(lang: Lang): lang is Exclude<Lang, 'plain'> {
  return (
    lang !== 'plain' &&
    !!shiki &&
    shiki.getLoadedLanguages().includes(SHIKI[lang])
  );
}

/** Kicks off loading `lang`'s grammar if it isn't loaded or loading yet;
 *  fire-and-forget — callers keep using the regex fallback until the next
 *  render after it lands (via the `version` bump, see `useShikiVersion`). */
function ensureLang(lang: Lang): void {
  if (lang === 'plain' || !shiki) return;
  const id = SHIKI[lang];
  if (shiki.getLoadedLanguages().includes(id) || pendingLangs.has(id)) return;
  const core = shiki;
  const p = core.loadLanguage(LANG_LOADER[lang]()).then(notify);
  pendingLangs.set(id, p);
}

/** One line, tokenized alone — the instant fallback, and the single-line
 *  call sites (a patch preview, a symbol-viewer line) that don't carry
 *  cross-line grammar state anyway. */
export function tokens(src: string, lang: Lang): Tok[] {
  if (!langReady(lang)) {
    ensureLang(lang);
    return regexTokens(src, lang);
  }
  const key = `${lang}\0${src}`;
  let t = cache.get(key);
  if (!t) {
    const [line = []] = shiki!.codeToTokensBase(src, {
      lang: SHIKI[lang],
      theme: 'heroui',
    });
    t = line.map((x) => ({
      s: x.content,
      c: x.color,
      i: !!((x.fontStyle ?? 0) & 1),
    }));
    cache.set(key, t);
  }
  return t;
}

/** A multi-line block, tokenized in one Shiki call so grammar state (an
 *  open JSDoc-style comment, a template literal, a Python docstring) carries
 *  from one line to the next — the fix for per-line tokenizing losing it.
 *  Returns one `Tok[]` per line of `src`, in order. */
export function tokenizeLines(src: string, lang: Lang): Tok[][] {
  if (!langReady(lang)) {
    ensureLang(lang);
    return src.split('\n').map((l) => regexTokens(l, lang));
  }
  const key = `${lang}\0${src}`;
  let t = blockCache.get(key);
  if (!t) {
    const lines = shiki!.codeToTokensBase(src, {
      lang: SHIKI[lang],
      theme: 'heroui',
    });
    t = lines.map((line) =>
      line.map((x) => ({
        s: x.content,
        c: x.color,
        i: !!((x.fontStyle ?? 0) & 1),
      })),
    );
    blockCache.set(key, t);
  }
  return t;
}

/** A hunk's lines, split into the side each belongs to: context and
 *  deletions on the old side, context and additions on the new side — the
 *  same pairing a unified diff is read with. Pure and diff-library-free so
 *  it's easy to unit test on its own. */
export function splitHunkSides<
  T extends { kind: 'ctx' | 'add' | 'del'; text: string },
>(
  lines: readonly T[],
): { oldIdx: number[]; newIdx: number[]; oldText: string; newText: string } {
  const oldIdx: number[] = [];
  const newIdx: number[] = [];
  lines.forEach((l, i) => {
    if (l.kind !== 'add') oldIdx.push(i);
    if (l.kind !== 'del') newIdx.push(i);
  });
  return {
    oldIdx,
    newIdx,
    oldText: oldIdx.map((i) => lines[i]!.text).join('\n'),
    newText: newIdx.map((i) => lines[i]!.text).join('\n'),
  };
}

/** Per-line tokens for a whole hunk, tokenizing each side once instead of
 *  each line alone: context and added lines take the new side's tokens,
 *  deleted lines take the old side's — so a hunk inside a multi-line
 *  comment or string keeps its grammar's state, in either view. */
export function hunkLineTokens<
  T extends { kind: 'ctx' | 'add' | 'del'; text: string },
>(lines: readonly T[], lang: Lang): Tok[][] {
  const { oldIdx, newIdx, oldText, newText } = splitHunkSides(lines);
  const oldToks = tokenizeLines(oldText, lang);
  const newToks = tokenizeLines(newText, lang);
  const out: Tok[][] = new Array(lines.length);
  // New-side tokens first (covers context and added lines) …
  newIdx.forEach((li, k) => {
    out[li] = newToks[k] ?? [];
  });
  // … then old-side tokens overwrite deleted lines only, so context keeps
  // the new side's render (the rule the brief calls out explicitly).
  oldIdx.forEach((li, k) => {
    if (lines[li]!.kind === 'del') out[li] = oldToks[k] ?? [];
  });
  return out;
}

const subscribe = (f: () => void) => {
  listeners.add(f);
  return () => listeners.delete(f);
};

/** Re-renders whenever Shiki (or a new grammar) finishes loading — so a
 *  hunk's `useMemo`'d block tokens get recomputed once the real grammar is
 *  in, and `Code`'s own single-line fallback repaints the same way. */
export function useShikiVersion(): number {
  return useSyncExternalStore(subscribe, () => version);
}

/** Languages (and markdown fence names) where `#` opens a comment, so a
 *  `#` after whitespace isn't read as a hex colour (see `findColors`).
 *  Plain text is in: unknown files are mostly configs with `#` comments. */
const HASH_COMMENTS = new Set([
  'python',
  'py',
  'shell',
  'sh',
  'bash',
  'zsh',
  'ruby',
  'rb',
  'yaml',
  'yml',
  'toml',
  'dockerfile',
  'makefile',
  'perl',
  'r',
  'plain',
]);

/** The colour literals in `s`, code in `lang` (a `Lang` or a fence name). */
export function colorsIn(s: string, lang = ''): ColorHit[] {
  return findColors(s, HASH_COMMENTS.has(lang.toLowerCase()));
}

/** A small square of the literal's colour, drawn just before it. Pure
 *  decoration: no text (so a copy never includes it), not selectable,
 *  hidden from assistive tech; `ColorPreviewHost` shows the big preview on
 *  hover, from `data-color`. */
function Swatch({ color }: { color: string }) {
  return (
    <span
      className="cswatch"
      aria-hidden
      data-color={color}
      style={{ '--sw': color } as CSSProperties}
    />
  );
}

/** `text` with a swatch before each colour literal — for code that isn't
 *  tokenized (markdown code spans and fenced blocks). */
export function withSwatches(text: string, lang?: string): ReactNode {
  const hits = colorsIn(text, lang);
  if (!hits.length) return text;
  const out: ReactNode[] = [];
  let pos = 0;
  hits.forEach((h, i) => {
    out.push(text.slice(pos, h.start), <Swatch key={i} color={h.text} />);
    pos = h.start;
  });
  out.push(text.slice(pos));
  return out;
}

/** One line of code, highlighted; `marks` are character ranges changed
 *  within the line. `toks`, when given, are used instead of tokenizing `s`
 *  alone — pass a hunk's (or a gap's) per-line block tokens here so
 *  multi-line grammar state (a comment, a string) is right. Colour
 *  literals get a swatch in front, found on the whole line rather than per
 *  token: a grammar splits `rgb(…)` into several tokens. */
export function Code({
  s,
  lang,
  marks,
  toks,
}: {
  s: string;
  lang: Lang;
  marks?: [number, number][];
  toks?: Tok[];
}) {
  useShikiVersion();
  const colors = colorsIn(s, lang);
  const out: ReactNode[] = [];
  let pos = 0;
  let k = 0;
  let ci = 0;
  for (const t of toks ?? tokens(s, lang)) {
    const end = pos + t.s.length;
    const cuts = [pos, end];
    for (const [a, b] of marks ?? []) {
      if (a > pos && a < end) cuts.push(a);
      if (b > pos && b < end) cuts.push(b);
    }
    for (const c of colors)
      if (c.start > pos && c.start < end) cuts.push(c.start);
    cuts.sort((a, b) => a - b);
    for (let n = 0; n < cuts.length - 1; n++) {
      const a = cuts[n]!;
      const b = cuts[n + 1]!;
      if (a === b) continue;
      // Every literal's start is a cut, so pieces meet them in order.
      if (colors[ci]?.start === a)
        out.push(<Swatch key={k++} color={colors[ci++]!.text} />);
      const hit = marks?.some(([x, y]) => a >= x && a < y);
      const text = s.slice(a, b);
      out.push(
        t.c || t.i || hit ? (
          <span
            key={k++}
            className={hit ? 'wd' : undefined}
            style={{ color: t.c, fontStyle: t.i ? 'italic' : undefined }}
          >
            {text}
          </span>
        ) : (
          text
        ),
      );
    }
    pos = end;
  }
  return <code className="src">{out}</code>;
}

/** `code` spans, **bold** and @mentions in chat / comment text. */
export function rich(text: string): ReactNode[] {
  return text
    .split(/(`[^`]+`|\*\*[^*]+\*\*|@[\w-]+(?:\/[\w-]+)?)/g)
    .map((p, i) =>
      p.startsWith('`') ? (
        <code key={i}>{withSwatches(p.slice(1, -1))}</code>
      ) : p.startsWith('**') ? (
        <b key={i}>{p.slice(2, -2)}</b>
      ) : p.startsWith('@') && p.length > 1 ? (
        <span key={i} className="mention font-medium text-accent">
          {p}
        </span>
      ) : (
        p
      ),
    );
}

/** GitHub-flavoured markdown (headings, lists, tables, task lists, code,
 *  footnotes) for PR descriptions, comments and agent replies. Raw HTML goes
 *  through GitHub's own allowlist (`<details>`, `<summary>`, `<kbd>`,
 *  `<sub>`, `<br>`…); scripts, styles and event handlers never render. */
export const Md = memo(function Md({ text }: { text: string }) {
  return (
    <div className="rich md">
      <Markdown
        remarkPlugins={[remarkGfm]}
        rehypePlugins={[rehypeRaw, [rehypeSanitize, defaultSchema]]}
        components={mdComponents}
      >
        {stripHtmlComments(text)}
      </Markdown>
    </div>
  );
});

/** HTML comments (e.g. Theodore's idempotency markers) never show. */
export function stripHtmlComments(text: string): string {
  return text.replace(/<!--[\s\S]*?-->/g, '').trimEnd();
}

const mdComponents: Components = {
  pre: ({ children }) => (
    <pre className="my-2 overflow-x-auto rounded-xl bg-default px-3 py-2 font-mono text-xs">
      {children}
    </pre>
  ),
  // Inline spans and fenced blocks alike; a block's fence names its language.
  code: ({ className, children }) => (
    <code className={className}>
      {typeof children === 'string'
        ? withSwatches(children, /language-(\S+)/.exec(className ?? '')?.[1])
        : children}
    </code>
  ),
  table: ({ children }) => (
    <div className="my-2 overflow-x-auto">
      <table className="md-table">{children}</table>
    </div>
  ),
  a: ({ href, children }) => (
    <a
      href={href}
      className="text-accent underline-offset-2 hover:underline"
      target="_blank"
      rel="noreferrer"
    >
      {children}
    </a>
  ),
};
