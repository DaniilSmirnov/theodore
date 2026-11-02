/**
 * The ⌘F search field shown over a list (Inbox / My PRs, or the sidebar's
 * Spaces section) — one component so the Esc / ↑↓ / ⏎ behavior is the same
 * everywhere it shows up. The caller owns the query and the filtered
 * order; this just renders the field and turns its keys into calls.
 */
import { useEffect, useRef } from 'react';
import { Magnifier } from '@gravity-ui/icons';
import { SearchField } from '@heroui/react';

export function ListSearchBar({
  query,
  onQueryChange,
  order,
  selected,
  onSelect,
  onOpen,
  onClose,
  placeholder,
  /** Bumped by the caller each time ⌘F (re)opens this field, so it
   *  refocuses even when it was already open. */
  reopenToken,
}: {
  query: string;
  onQueryChange: (q: string) => void;
  /** Ids of the rows currently shown, in display order. */
  order: string[];
  selected: string | null;
  onSelect: (id: string) => void;
  onOpen: (id: string) => void;
  onClose: () => void;
  placeholder: string;
  reopenToken: number;
}) {
  const ref = useRef<HTMLInputElement>(null);
  useEffect(() => {
    ref.current?.focus();
  }, [reopenToken]);

  const move = (d: number) => {
    const i = selected ? order.indexOf(selected) : -1;
    const next = order[Math.max(0, Math.min(order.length - 1, i + d))];
    if (next) onSelect(next);
  };

  return (
    <div className="listsearch flex items-center gap-2 border-b border-separator px-3 py-2">
      <SearchField
        aria-label={placeholder}
        value={query}
        onChange={onQueryChange}
        fullWidth
      >
        <SearchField.Group>
          <SearchField.SearchIcon>
            <Magnifier />
          </SearchField.SearchIcon>
          <SearchField.Input
            ref={ref}
            placeholder={placeholder}
            onKeyDown={(e) => {
              if (e.key === 'Escape') {
                if (query) onQueryChange('');
                else onClose();
              } else if (e.key === 'ArrowDown') {
                move(1);
              } else if (e.key === 'ArrowUp') {
                move(-1);
              } else if (e.key === 'Enter') {
                if (selected && order.includes(selected)) onOpen(selected);
              } else {
                return;
              }
              e.preventDefault();
            }}
          />
          <SearchField.ClearButton />
        </SearchField.Group>
      </SearchField>
      <span className="shrink-0 whitespace-nowrap text-xs text-muted">
        {order.length} match{order.length === 1 ? '' : 'es'}
      </span>
    </div>
  );
}
