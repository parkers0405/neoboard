import { createInterface } from 'node:readline/promises';
import { offlineClient } from './fixture.ts';
import { review } from './propose.ts';

const outcome = await review(offlineClient(), async preview => {
  console.log(`Proposed fixture write: ${preview}`);
  if (!process.stdin.isTTY) return false;
  const terminal = createInterface({ input: process.stdin, output: process.stdout });
  try { return await terminal.question('Type APPROVE to simulate this reply: ') === 'APPROVE'; }
  finally { terminal.close(); }
});
console.log(outcome ? `Simulated reply ${outcome.id}.` : 'No action sent.');