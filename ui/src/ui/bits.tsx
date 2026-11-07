import {
  Avatar as HAvatar,
  Button,
  Chip,
  Kbd,
  Modal,
  Spinner as HSpinner,
  Toolbar,
} from '@heroui/react';
import type { ComponentProps, ReactNode } from 'react';
import { CircleCheckFill, CircleXmarkFill, Clock } from '@gravity-ui/icons';
import type { AgentId, CheckRollup, Person, Severity } from '../gen/contract';

export function Avatar({
  person,
  size = 18,
}: {
  person: Person;
  size?: number;
}) {
  const label = person.name ?? person.login;
  return (
    <HAvatar
      className="shrink-0 rounded-full"
      style={{ width: size, height: size }}
      aria-label={label}
      variant="soft"
      color="default"
    >
      {person.avatar_url && <HAvatar.Image src={person.avatar_url} />}
      <HAvatar.Fallback
        className="font-semibold"
        style={{ fontSize: size * 0.5 }}
      >
        {label[0]?.toUpperCase() ?? '?'}
      </HAvatar.Fallback>
    </HAvatar>
  );
}

export const agentText: Record<AgentId, string> = {
  claude: 'text-foreground',
  codex: 'text-foreground',
  gemini: 'text-foreground',
};

export function AgentTag({ id }: { id: AgentId }) {
  return (
    <Chip
      size="sm"
      variant="secondary"
      className={`agent-tag font-mono ${agentText[id]}`}
    >
      {id}
    </Chip>
  );
}

export function Sev({ s }: { s: Severity }) {
  return (
    <Chip
      size="sm"
      variant="soft"
      color={s === 'high' ? 'danger' : s === 'warn' ? 'warning' : 'default'}
    >
      {s}
    </Chip>
  );
}

export function CheckIcon({ s }: { s: CheckRollup }) {
  if (s === 'none') return <span />;
  if (s === 'pass')
    return (
      <CircleCheckFill
        className="size-3.5 text-success"
        aria-label="checks green"
      />
    );
  if (s === 'fail')
    return (
      <CircleXmarkFill
        className="size-3.5 text-danger"
        aria-label="checks failed"
      />
    );
  return (
    <Clock className="size-3.5 text-warning" aria-label="checks running" />
  );
}

export function Spinner() {
  return (
    <HSpinner
      size="sm"
      color="current"
      className="inline-block size-3 align-[-2px]"
    />
  );
}

export type Tone =
  | 'primary'
  | 'secondary'
  | 'tertiary'
  | 'ghost'
  | 'outline'
  | 'danger'
  | 'danger-soft';

/** Button whose variant says what it does; its shortcut is printed on it. */
export function Btn({
  tone = 'tertiary',
  kbd,
  icon,
  children,
  className = '',
  ...rest
}: { tone?: Tone; kbd?: string; icon?: ReactNode } & Omit<
  ComponentProps<typeof Button>,
  'variant' | 'children'
> & { children?: ReactNode }) {
  return (
    <Button
      size="sm"
      variant={tone}
      className={`gap-1.5 ${className}`}
      {...rest}
    >
      {icon}
      {children}
      {kbd && (
        <Kbd
          variant="light"
          className="ms-0.5 h-4 min-w-4 bg-current/10 px-1 text-[10px] text-current opacity-70"
        >
          {kbd}
        </Kbd>
      )}
    </Button>
  );
}

export function K({ children }: { children: ReactNode }) {
  return <Kbd className="text-xs">{children}</Kbd>;
}

/** The shortcut strip under a page: one line; on a narrow window the
 *  hints that don't fit fade out at the edge instead of wrapping. */
export function Hints({ children }: { children: ReactNode }) {
  return (
    <Toolbar
      aria-label="Shortcuts"
      // Narrow, the keys wrap onto a second line rather than run off the
      // edge; each label stays whole (a flex item of its own).
      className="hints flex w-full shrink-0 flex-wrap items-center gap-x-1 gap-y-1 whitespace-nowrap border-t border-separator bg-background px-6 py-2 text-xs text-muted"
    >
      {children}
    </Toolbar>
  );
}

/** Shared modal frame — same shell every dialog in the app uses. */
export function Shell({
  title,
  children,
  size = 'md',
  footer,
  onClose,
}: {
  title: string;
  children: ReactNode;
  size?: 'md' | 'lg';
  footer?: ReactNode;
  onClose: () => void;
}) {
  return (
    <Modal.Backdrop isOpen onOpenChange={(open) => !open && onClose()}>
      <Modal.Container
        size={size}
        placement="top"
        scroll="inside"
        className="mt-[8vh]"
      >
        <Modal.Dialog
          aria-label={title}
          className={`modal flex max-h-[84vh] flex-col gap-3 p-4 ${size === 'lg' ? 'w-[760px] max-w-[92vw]' : 'w-[520px] max-w-[92vw]'}`}
        >
          <Modal.CloseTrigger aria-label="Close" />
          <Modal.Heading className="shrink-0 pe-8">{title}</Modal.Heading>
          {/* The body scrolls; the title and footer stay put. */}
          <div className="-mx-4 -my-1 flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto px-4 py-1">
            {children}
          </div>
          {footer && (
            <div className="flex shrink-0 justify-end gap-2">{footer}</div>
          )}
        </Modal.Dialog>
      </Modal.Container>
    </Modal.Backdrop>
  );
}

export function Empty({
  icon,
  title,
  text,
  action,
}: {
  icon: ReactNode;
  title: string;
  text?: string;
  action?: ReactNode;
}) {
  return (
    <div className="empty flex min-w-0 flex-1 flex-col items-center justify-center gap-3 p-10 text-center">
      <span className="grid size-12 place-items-center rounded-full bg-default text-muted [&>svg]:size-5">
        {icon}
      </span>
      <span className="text-base font-semibold">{title}</span>
      {text && <span className="max-w-sm text-sm text-muted">{text}</span>}
      {action}
    </div>
  );
}
