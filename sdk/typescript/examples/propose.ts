import type { BoardClient } from '../index.ts';

export async function review(client: Pick<BoardClient, 'snapshot' | 'action'>, confirm: (preview: string) => Promise<boolean>) {
  const { messages } = await client.snapshot();
  const root = messages.find(message => message.parent === null);
  if (!root) return null;
  const proposal = { type: 'reply', thread: root.id, text: 'Draft reviewed.' } as const;
  if (!await confirm(JSON.stringify({ channel: root.channel, action: proposal }))) return null;
  return client.action(proposal);
}