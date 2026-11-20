// Wire names and optionality follow src/protocol.rs. Timestamps are Unix seconds.
export interface Task {
  state: 'ready' | 'waiting' | 'claimed' | 'blocked' | 'completed' | 'failed' | 'canceled';
  needs: number[];
  assignee: string | null;
  owner: string | null;
  finished_by?: string;
  result?: string;
  expires_at: number | null;
}
export interface Message {
  id: number;
  channel: string;
  parent: number | null;
  author: string;
  sender?: string;
  text: string;
  labels: Record<string, string>;
  created_at: number;
  task: Task | null;
  attachments?: { id: number; message_id: number; name: string; size: number }[];
}
export interface Snapshot { cursor: number; channels: string[]; messages: Message[] }
export interface Sprint { id: number; board: string; title: string; description: string; finished: boolean }
export interface SprintCatalog { sprints: Record<string, Sprint>; task_sprints: Record<string, number> }
export interface Member { actor: string; display_name: string | null }
export interface TaskPlacement { parent: number | null; global: boolean; done_by: string[] }
export type BoardEvent = { cursor: number; created_at: number; actor: string } & (
  | { type: 'message'; message: Message }
  | { type: 'organization_changed'; board: string }
  | { type: 'channel_created' | 'channel_deleted'; channel: string }
  | { type: 'thread_deleted' | 'schedule_canceled'; id: number; channel: string }
  | { type: 'reaction'; id: number; channel: string; reaction: string; remove: boolean }
  | { type: 'scheduled'; id: number; channel: string; publish_at: number }
  | { type: 'schedule_published'; id: number; channel: string; message_id: number }
);
export type Action =
  | { type: 'create_sprint'; board: string; title: string; description: string }
  | { type: 'post_sprint_task'; channel: string; text: string; labels?: Record<string, string>; needs?: number[]; assignee?: string; sprint: number }
  | { type: 'set_task_sprint'; id: number; sprint: number | null }
  | { type: 'finish_sprint'; id: number; allow_unfinished: boolean }
  | { type: 'post'; channel: string; text: string; labels?: Record<string, string> }
  | { type: 'reply'; thread: number; text: string }
  | { type: 'post_task'; channel: string; text: string; labels?: Record<string, string>; needs?: number[]; assignee?: string; parent?: number; global?: boolean }
  | { type: 'claim' | 'renew'; id: number; lease_secs: number }
  | { type: 'complete' | 'fail' | 'block'; id: number; text: string };
export interface Outcome { id: number | null; cursor: number }