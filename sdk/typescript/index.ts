import type { Action, BoardEvent, Member, TaskPlacement, Outcome, Snapshot, SprintCatalog } from './protocol.ts';
export type * from './protocol.ts';

export interface ClientOptions {
  endpoint: string;
  token: string;
  fetch?: typeof fetch;
}

export class BoardError extends Error {
  readonly status?: number;
  constructor(message: string, status?: number) {
    super(message);
    this.name = 'BoardError';
    this.status = status;
  }
}

export class BoardClient {
  #base: URL;
  #token: string;
  #fetch: typeof fetch;

  constructor(options: ClientOptions) {
    let url: URL;
    try { url = new URL(options.endpoint); } catch { throw new BoardError('Invalid board endpoint'); }
    const local = /^(localhost|127\.\d+\.\d+\.\d+|\[::1\])$/.test(url.hostname);
    if (!(url.protocol === 'https:' || (local && url.protocol === 'http:')) ||
        url.username || url.password || /[?#]/.test(url.href)) throw new BoardError('Invalid board endpoint');
    if (!url.pathname.endsWith('/')) url.pathname += '/';
    if (!/^[\x21-\x7e]+$/.test(options.token) || !options.token) throw new BoardError('Invalid board credential');
    this.#base = url;
    this.#token = options.token;
    this.#fetch = options.fetch ?? globalThis.fetch;
  }

  async #request<T>(path: string, action?: Action, key?: string): Promise<T> {
    const headers: Record<string, string> = { Authorization: `Bearer ${this.#token}` };
    if (action !== undefined) headers['Content-Type'] = 'application/json';
    if (key !== undefined) headers['Idempotency-Key'] = key;
    let response: Response;
    try {
      response = await this.#fetch(new URL(path, this.#base), {
        method: action === undefined ? 'GET' : 'POST', headers,
        body: action === undefined ? undefined : JSON.stringify(action),
        redirect: 'error', credentials: 'omit', cache: 'no-store', signal: AbortSignal.timeout(35000),
      });
    } catch { throw new BoardError('Board request failed'); }
    if (!response.ok) throw new BoardError(`Board returned HTTP ${response.status}`, response.status);
    try { return await response.json() as T; } catch { throw new BoardError('Invalid board JSON'); }
  }

  me() { return this.#request<Member>('v1/me'); }
  snapshot() { return this.#request<Snapshot>('v1/snapshot'); }
  async sprintCatalog(): Promise<SprintCatalog> {
    const { sprints = {}, task_sprints = {} } = await this.#request<Partial<SprintCatalog>>('v1/organization');
    return { sprints, task_sprints };
  }
  async taskPlacements(): Promise<Record<string, TaskPlacement>> {
    const { tasks } = await this.#request<{ tasks: Record<string, TaskPlacement> }>('v1/organization');
    return Object.fromEntries(Object.entries(tasks).map(([id, { parent, global, done_by }]) => [id, { parent, global, done_by }]));
  }
  members(channel: string) { return this.#request<Member[]>('v1/members?' + new URLSearchParams({ channel })); }
  events(after: number, waitMs = 0) {
    if (!Number.isSafeInteger(after) || after < 0 || !Number.isInteger(waitMs) || waitMs < 0 || waitMs > 30000)
      throw new BoardError('Invalid event cursor or wait time');
    return this.#request<BoardEvent[]>(`v1/events?after=${after}&wait_ms=${waitMs}`);
  }
  action(action: Action, idempotencyKey?: string) {
    return this.#request<Outcome>('v1/actions', action, idempotencyKey);
  }
}