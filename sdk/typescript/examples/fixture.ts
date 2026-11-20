import { BoardClient, type Action, type Snapshot } from '../index.ts';

// A single credential's view, not a board emulator. No fallback to network fetch.
const snapshot: Snapshot = {
  cursor: 12, channels: ['demo/general'], messages: [{
    id: 7, channel: 'demo/general', parent: null, author: 'planner', text: 'Please review the draft.',
    labels: {}, created_at: 1700000000, task: null,
  }],
};
export function offlineClient() {
  return new BoardClient({ endpoint: 'https://board.invalid', token: 'fixture-only', fetch: async (input, init) => {
    const path = String(input).replace('https://board.invalid', '');
    if (init?.method === 'GET' && path === '/v1/snapshot') return Response.json(snapshot);
    if (init?.method === 'GET' && path === '/v1/events?after=12&wait_ms=0') return Response.json([]);
    if (init?.method === 'POST' && path === '/v1/actions') {
      const action: Action = JSON.parse(String(init.body));
      if (action.type === 'reply' && action.thread === 7) return Response.json({ id: 8, cursor: 14 });
    }
    return new Response(null, { status: 404 });
  } });
}