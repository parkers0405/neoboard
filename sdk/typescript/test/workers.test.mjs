import test from 'node:test';
import assert from 'node:assert/strict';
import { workerFixture } from '../examples/worker-fixture.ts';
import { workOnce } from '../examples/work-once.ts';

test('two stale snapshots yield one worker; renewal spans the original deadline', async () => {
  const { alpha, beta, advance } = workerFixture();
  let runs = 0;
  const run = client => workOnce(client, async () => advance(6), () => ++runs);
  assert.deepEqual((await Promise.all([run(alpha), run(beta)])).sort(), ['completed', 'lost race']);
  assert.equal(await run(beta), 'idle');
  assert.equal(runs, 1);
});

test('expired worker stops before work; another identity can reclaim', async () => {
  const { alpha, beta, advance } = workerFixture();
  assert.equal(await workOnce(alpha, async () => advance(11), () => assert.fail('stale work')), 'lost lease');
  assert.equal(await workOnce(beta, async () => advance(1)), 'completed');
  const [row] = (await alpha.snapshot()).messages;
  assert.equal(row.task.finished_by, 'beta');
  assert.equal(row.task.result, '3 words');
});

test('scoped reads and writes exclude private job; non-owner cannot renew or finish', async () => {
  const { alpha, beta } = workerFixture();
  for (const client of [alpha, beta]) {
    const snapshot = await client.snapshot();
    assert.deepEqual(snapshot.channels, ['demo/jobs']);
    assert.deepEqual(snapshot.messages.map(m => m.id), [1]);
    await assert.rejects(client.action({ type: 'claim', id: 2, lease_secs: 10 }), { status: 404 });
  }
  await alpha.action({ type: 'claim', id: 1, lease_secs: 10 });
  await assert.rejects(beta.action({ type: 'renew', id: 1, lease_secs: 10 }), { status: 409 });
  await assert.rejects(beta.action({ type: 'complete', id: 1, text: 'wrong owner' }), { status: 409 });
});