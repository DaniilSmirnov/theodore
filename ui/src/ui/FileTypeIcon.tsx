/**
 * A small muted mark for a file's type: its exact name, then its extension,
 * then a plain file. Letters in the theme's own tokens rather than logos, so
 * every type reads alike in light and dark; drawn from an attribute, so
 * selecting a row's text doesn't pick them up.
 */
import { memo } from 'react';
import { Cube, File, Lock } from '@gravity-ui/icons';

const BY_NAME: Record<string, FileType> = {
  dockerfile: 'dockerfile',
  'cargo.lock': 'lock',
  'pnpm-lock.yaml': 'lock',
  'package-lock.json': 'lock',
  'yarn.lock': 'lock',
};

const GLYPHS = {
  rs: 'RS',
  ts: 'TS',
  tsx: 'TX',
  js: 'JS',
  jsx: 'JX',
  json: '{}',
  md: 'MD',
  yaml: 'YM',
  toml: 'TM',
  py: 'PY',
  go: 'GO',
  css: '#',
  html: '<>',
  sh: '$',
} as const;

type FileType = keyof typeof GLYPHS | 'lock' | 'dockerfile';

/** The file's type; `null`: nothing known, a plain file. */
export function fileType(path: string): FileType | null {
  const name = (path.split('/').pop() ?? path).toLowerCase();
  const exact = Object.hasOwn(BY_NAME, name) ? BY_NAME[name] : undefined;
  if (exact) return exact;
  if (name.startsWith('dockerfile.')) return 'dockerfile';
  const ext = name.includes('.') ? name.split('.').pop()! : '';
  if (ext === 'yml') return 'yaml';
  if (ext === 'lock') return 'lock';
  return Object.hasOwn(GLYPHS, ext) ? (ext as keyof typeof GLYPHS) : null;
}

export const FileTypeIcon = memo(function FileTypeIcon({
  path,
}: {
  path: string;
}) {
  const type = fileType(path);
  if (type == null) {
    return (
      <File data-file-type="file" className="size-3.5 shrink-0 text-muted" />
    );
  }
  if (type === 'lock') {
    return (
      <Lock data-file-type={type} className="size-3 shrink-0 text-muted" />
    );
  }
  if (type === 'dockerfile') {
    return (
      <Cube data-file-type={type} className="size-3 shrink-0 text-muted" />
    );
  }
  return (
    <span
      aria-hidden
      data-file-type={type}
      data-glyph={GLYPHS[type]}
      className="inline-grid h-3.5 min-w-3.5 shrink-0 place-items-center rounded-[3px] border border-separator px-px font-mono text-[7px] font-semibold leading-none text-muted before:content-[attr(data-glyph)]"
    />
  );
});
