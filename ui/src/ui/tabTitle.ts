/** Review tab titles that outlive their PR row (an archived space's branch
 *  review is closed with it): the last title shown, else the repo's name
 *  and number, never the raw PR id. */
const last = new Map<string, string>();

export function reviewTabTitle(pr: string, title: string | null): string {
  if (title) {
    last.set(pr, title);
    return title;
  }
  const kept = last.get(pr);
  if (kept) return kept;
  const m = /^[^:]+:(.+)#(\d+)$/.exec(pr);
  if (!m) return pr;
  const repo = m[1]!.split('/').filter(Boolean).pop() ?? m[1]!;
  return `${repo} #${m[2]}`;
}
