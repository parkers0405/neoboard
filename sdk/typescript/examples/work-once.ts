import { BoardClient, BoardError } from '../index.ts';

// One bounded iteration. Only a successful claim authorizes starting work.
export async function workOnce(client: BoardClient, checkpoint: () => Promise<void>, countWords = (text: string) => text.trim().split(/\s+/u).filter(Boolean).length) {
  const row = (await client.snapshot()).messages.find(m => m.task?.state === 'ready' && m.task.assignee === null);
  if (!row) return 'idle';
  const lease = { id: row.id, lease_secs: 10 };
  try {
    await client.action({ type: 'claim', ...lease });
  } catch (err) {
    if (err instanceof BoardError && err.status === 409) return 'lost race';
    throw err;
  }

  // Simulate two bounded stages. A stalled worker may wake after its lease expires.
  await checkpoint();
  try {
    await client.action({ type: 'renew', ...lease });
    const words = countWords(row.text); // Pure, safe to repeat after a crash.
    await checkpoint();
    await client.action({ type: 'complete', id: row.id, text: `${words} words` });
    return 'completed';
  } catch (err) {
    if (err instanceof BoardError && err.status === 409) return 'lost lease';
    throw err; // No speculative finish or blind retry on an ambiguous outcome.
  }
}