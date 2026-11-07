/**
 * `WireRow.model` / `MutationOp.model` are plain strings on the wire (see
 * `crates/contract`); this is the UI's side of that contract — the set of
 * model names it knows how to pool, and the row shape each one carries.
 *
 * The names match `theodore_contract::models` (Rust). `pr`, `pr_detail` and
 * `grouping` rows are keyed by the PR id; threads live inside `pr_detail`.
 */
import type {
  ChatMessage,
  CheckTick,
  Draft,
  FileViewedRow,
  Finding,
  Grouping,
  HunkRead,
  InboxState,
  PrDetail,
  PrSummary,
  Project,
  Space,
  Setting,
  SkillRun,
  Task,
} from '../gen/contract';

export const MODEL_NAMES = [
  'pr',
  'pr_detail',
  'grouping',
  'inbox_state',
  'hunk_read',
  'file_viewed',
  'check_tick',
  'draft',
  'finding',
  'task',
  'skill_run',
  'chat_message',
  'setting',
  'project',
  'space',
] as const;

export type ModelName = (typeof MODEL_NAMES)[number];

/** A row as the pool stores it: the typed payload, plus the pool's own key
 *  always stamped on as `id` — `PrSummary` and `Setting` carry no id field
 *  of their own, so this is the only place it survives. */
type WithId<T> = T & { id: string };

/** Row shape stored in the pool for each model name. */
export interface ModelRows {
  pr: WithId<PrSummary>;
  pr_detail: WithId<PrDetail>;
  grouping: WithId<Grouping>;
  inbox_state: WithId<InboxState>;
  hunk_read: WithId<HunkRead>;
  file_viewed: WithId<FileViewedRow>;
  check_tick: WithId<CheckTick>;
  draft: WithId<Draft>;
  finding: WithId<Finding>;
  task: WithId<Task>;
  skill_run: WithId<SkillRun>;
  chat_message: WithId<ChatMessage>;
  setting: WithId<Setting>;
  project: WithId<Project>;
  space: WithId<Space>;
}

/**
 * The pool is keyed by whatever string core assigns: `WireRow.id` on
 * bootstrap/sync, `MutationOp.id` on a local write. Models without a natural
 * single-field id (`inbox_state`, `hunk_read`, `file_viewed`, `setting`) are
 * expected to use a composite string (e.g. `"<pr>:<hunk>"`) — core and the
 * UI must agree on that string since it is never recomputed locally.
 */
