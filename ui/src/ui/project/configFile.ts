/**
 * Project settings → Config: pure rules for the config editor — which
 * text it starts from, Tab / Enter in a plain textarea, the problems list,
 * the "Save to…" menu, and the toasts after a save.
 */
import type {
  ConfigDraft,
  ConfigLayer,
  ConfigProblem,
  SaveConfigResult,
} from '../../gen/contract';

/** YAML indents with spaces; two is what the generated file uses. */
export const INDENT = '  ';

/** Where the editor's text came from: a layer's file, or the settings. */
export type ConfigSource = ConfigLayer | 'generated';

/** The text and source the editor opens with: the repo's file, else the
 *  local one, else YAML generated from the current settings. */
export function startOf(draft: ConfigDraft): {
  source: ConfigSource;
  text: string;
} {
  if (draft.initial) {
    const file = draft.files.find((f) => f.layer === draft.initial);
    if (file?.text != null) return { source: draft.initial, text: file.text };
  }
  return { source: 'generated', text: draft.generated };
}

/** The text of `source` in `draft` (`null`: that file doesn't exist). */
export function textOf(
  draft: ConfigDraft,
  source: ConfigSource,
): string | null {
  if (source === 'generated') return draft.generated;
  return draft.files.find((f) => f.layer === source)?.text ?? null;
}

/** The layer previewed for a source: a file previews in its own layer,
 *  generated text in the repo layer (the team's file is the usual home). */
export function previewLayer(source: ConfigSource): ConfigLayer {
  return source === 'generated' ? 'repo' : source;
}

export interface Edit {
  text: string;
  start: number;
  end: number;
}

/** Tab / Shift+Tab: with no selection, Tab inserts `INDENT` at the
 *  cursor; with a selection (or Shift), every touched line is indented or
 *  dedented by one level and the selection follows. */
export function tabEdit(
  text: string,
  start: number,
  end: number,
  shift: boolean,
): Edit {
  if (!shift && start === end) {
    return {
      text: text.slice(0, start) + INDENT + text.slice(end),
      start: start + INDENT.length,
      end: start + INDENT.length,
    };
  }
  const lineStart = text.lastIndexOf('\n', start - 1) + 1;
  // A selection ending right after a newline doesn't touch the next line.
  const last = end > start && text[end - 1] === '\n' ? end - 1 : end;
  const nl = text.indexOf('\n', last);
  const lineEnd = nl === -1 ? text.length : nl;
  const lines = text.slice(lineStart, lineEnd).split('\n');
  let first = 0;
  let total = 0;
  const changed = lines.map((line, i) => {
    if (!shift) {
      if (i === 0) first = INDENT.length;
      total += INDENT.length;
      return INDENT + line;
    }
    const strip = line.startsWith(INDENT)
      ? INDENT.length
      : line.startsWith(' ')
        ? 1
        : 0;
    if (i === 0) first = -strip;
    total -= strip;
    return line.slice(strip);
  });
  return {
    text: text.slice(0, lineStart) + changed.join('\n') + text.slice(lineEnd),
    start: Math.max(lineStart, start + first),
    end: Math.max(lineStart, end + total),
  };
}

/** Enter keeps the line's indentation, one level deeper after a `key:`
 *  that opens a block. */
export function newlineEdit(text: string, start: number, end: number): Edit {
  const lineStart = text.lastIndexOf('\n', start - 1) + 1;
  const line = text.slice(lineStart, start);
  let indent = /^ */.exec(line)?.[0] ?? '';
  const code = line.replace(/\s+#.*$/, '').trimEnd();
  if (code.endsWith(':') && !code.trimStart().startsWith('#')) indent += INDENT;
  const insert = `\n${indent}`;
  return {
    text: text.slice(0, start) + insert + text.slice(end),
    start: start + insert.length,
    end: start + insert.length,
  };
}

export function hasErrors(problems: readonly ConfigProblem[]): boolean {
  return problems.some((p) => p.level === 'error');
}

/** "Line 4, column 9: …", or the message alone without a position. */
export function problemText(p: ConfigProblem): string {
  if (p.line == null) return p.message;
  const at =
    p.column != null ? `Line ${p.line}, column ${p.column}` : `Line ${p.line}`;
  return `${at}: ${p.message}`;
}

/** The character offset where 1-based `line` starts, for jumping there. */
export function offsetOfLine(text: string, line: number): number {
  let at = 0;
  for (let n = 1; n < line; n++) {
    const nl = text.indexOf('\n', at);
    if (nl === -1) return text.length;
    at = nl + 1;
  }
  return at;
}

export interface SaveOption {
  id: string;
  layer: ConfigLayer;
  commit: boolean;
  label: string;
  /** Where it writes. */
  detail: string;
  /** Saving replaces a file that exists. */
  replaces: boolean;
}

const LABEL: Record<ConfigLayer, string> = {
  repo: 'Repo (team)',
  local: 'Local (mine)',
  global: 'Global',
};

/** The "Save to…" menu: repo, repo and commit (a local folder that has no
 *  other uncommitted change), local, global. */
export function saveOptions(
  draft: ConfigDraft,
  isFolder: boolean,
): SaveOption[] {
  const out: SaveOption[] = [];
  for (const layer of ['repo', 'local', 'global'] as const) {
    const file = draft.files.find((f) => f.layer === layer);
    const detail = file?.save_to ?? '';
    const replaces = layer !== 'repo' || isFolder ? file?.text != null : false;
    out.push({
      id: layer,
      layer,
      commit: false,
      label: LABEL[layer],
      detail,
      replaces,
    });
    if (layer === 'repo' && isFolder && draft.can_commit) {
      out.push({
        id: 'repo-commit',
        layer,
        commit: true,
        label: `${LABEL.repo} and commit`,
        detail: 'commits only this file, with your hooks',
        replaces,
      });
    }
  }
  return out;
}

/** The toast after a save: what happened, plus a note when the repo's
 *  commands were marked allowed. */
export function savedText(r: SaveConfigResult): string {
  return r.trusted ? `${r.message}. Its commands are allowed.` : r.message;
}

/** The toast when the outbox opened (or found) a config PR. */
export function configPrToast(
  number: number,
  url: string,
  open?: (url: string) => void,
): { text: string; action?: { label: string; onAction: () => void } } {
  const text = `Config PR #${number} is open`;
  if (!url || !open) return { text };
  return { text, action: { label: 'Open', onAction: () => open(url) } };
}
