import { BoardClient, type Action, type Message, type Snapshot } from '../index.ts';

export function workerFixture() {
  let now = 1700000000;
  let cursor = 2;
  const queue = 'demo/jobs';
  const rows: Message[] = [queue, 'private/jobs'].map((channel, i) => ({
    channel, id: i + 1, parent: null, author: 'planner', text: 'Count these words', labels: {}, created_at: now,
    task: { state: 'ready', needs: [], assignee: null, owner: null, expires_at: null },
  }));
  const scopes = new Map(['alpha', 'beta'].map(actor => [`Bearer fixture-${actor}`, { actor, channel: queue }]));
  const reject = (status: number) => new Response(null, { status });
  const transport: typeof fetch = async (input, init) => {
    const scope = scopes.get(new Headers(init?.headers).get('authorization') ?? '');
    if (!scope) return reject(401);
    const visible = rows.filter(row => row.channel === scope.channel);
    const route = `${init?.method} ${input}`;
    if (route === 'GET https://board.invalid/v1/snapshot') {
      return Response.json({ cursor, channels: [scope.channel], messages: visible } satisfies Snapshot);
    }
    if (route !== 'POST https://board.invalid/v1/actions') return reject(404);
    const action: Action = JSON.parse(String(init?.body));
    if (action.type !== 'claim' && action.type !== 'start' && action.type !== 'renew' && action.type !== 'complete') return reject(403);
    const task = visible.find(row => row.id === action.id)?.task;
    if (!task) return reject(404);
    // Check and mutate in one synchronous step; never grant a lease from a snapshot alone.
    if (action.type === 'claim' ? task.owner !== null || !['ready', 'in_progress'].includes(task.state) : task.owner !== scope.actor) return reject(409);
    if (action.type === 'start') {
      task.started_at ??= now;
      task.state = 'in_progress';
    } else if (action.type === 'complete') {
      Object.assign(task, { state: 'completed', owner: null, expires_at: null, finished_by: scope.actor, result: action.text });
    } else if (action.type === 'claim' || action.type === 'renew') {
      if (!Number.isInteger(action.lease_secs) || action.lease_secs < 1 || action.lease_secs > 86400) return reject(400);
      if (task.state === 'ready') task.state = 'claimed';
      Object.assign(task, { owner: scope.actor, expires_at: now + action.lease_secs });
    }
    return Response.json({ id: action.id, cursor: ++cursor });
  };
  const client = (actor: string) => new BoardClient({ endpoint: 'https://board.invalid', token: `fixture-${actor}`, fetch: transport });
  return {
    alpha: client('alpha'), beta: client('beta'),
    advance(seconds: number) {
      now += seconds;
      for (const { task } of rows) {
        if (task?.expires_at != null && task.expires_at <= now) {
          if (task.state === 'claimed') task.state = 'ready';
          Object.assign(task, { owner: null, expires_at: null });
          ++cursor;
        }
      }
    },
  };
}