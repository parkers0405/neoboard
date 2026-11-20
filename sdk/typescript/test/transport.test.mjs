import test from 'node:test';
import assert from 'node:assert/strict';
import { BoardClient, BoardError } from '../index.ts';
import { review } from '../examples/propose.ts';
import { offlineClient } from '../examples/fixture.ts';

function client(fetch) { return new BoardClient({ endpoint: 'https://example.invalid/proxy', token: 'fixture', fetch }); }

test('scoped reads use the board routes, encoded query and bearer header', async () => {
  const calls = [];
  const member = { actor: 'planner', display_name: null };
  const snapshot = { cursor: 42, channels: ['a/b & c'], messages: [] };
  const placement = { parent: 2, global: false, done_by: [] };
  const org = { areas: {}, area_owners: {}, tasks: { 3: { ...placement, area: 'legacy' } } };
  const changes = [{ type: 'channel_created', channel: 'a/new', actor: 'planner', cursor: 48, created_at: 1234 }];
  const responses = [member, snapshot, org, [member], changes];
  const c = client(async (url, init) => {
    assert.equal(init.headers.Authorization, 'Bearer fixture');
    assert.equal(init.redirect, 'error');
    assert.equal(init.method, 'GET');
    assert.equal(init.body, undefined);
    calls.push(url.href);
    return Response.json(responses.shift());
  });
  assert.deepEqual(await c.me(), member);
  assert.deepEqual(await c.snapshot(), snapshot);
  assert.deepEqual(await c.taskPlacements(), { 3: placement });
  assert.deepEqual(await c.members('a/b & c'), [member]);
  assert.deepEqual(await c.events(snapshot.cursor, 30000), changes);
  assert.deepEqual(calls, ['me', 'snapshot', 'organization', 'members?channel=a%2Fb+%26+c', 'events?after=42&wait_ms=30000'].map(s => 'https://example.invalid/proxy/v1/' + s));
  for (const args of [[-1], [Number.MAX_SAFE_INTEGER + 1], [0, 30001]]) assert.throws(() => c.events(...args), BoardError);
});

test('writes send the tagged action directly and preserve nullable outcomes', async () => {
  const action = { type: 'complete', id: 17, text: 'Reviewed' };
  const c = client(async (url, init) => {
    assert.equal(url.pathname, '/proxy/v1/actions');
    assert.equal(init.method, 'POST');
    assert.deepEqual(init.headers, { Authorization: 'Bearer fixture', 'Content-Type': 'application/json', 'Idempotency-Key': 'approval-17' });
    assert.deepEqual(JSON.parse(init.body), action);
    return Response.json({ cursor: 50, id: null });
  });
  assert.deepEqual(await c.action(action, 'approval-17'), { cursor: 50, id: null });
});

test('sprint catalog projects only sprint records and explicit task membership', async () => {
  const catalog = { sprints: { 7: { id: 7, board: 'work', title: 'Release', description: '', finished: false } }, task_sprints: { 42: 7 } };
  const c = client(url => {
    assert.equal(url.pathname, '/proxy/v1/organization');
    return Response.json({ ...catalog, tasks: { 42: { area: 'legacy' } }, areas: {}, area_owners: {} });
  });
  assert.deepEqual(await c.sprintCatalog(), catalog);
});

test('reject unsafe endpoints before fetching; errors omit upstream content', async () => {
  for (const endpoint of ['http://example.org', 'http://127.0.0.1.invalid', 'https://user:pw@host', 'https://host/?secret', 'https://host/#secret']) {
    assert.throws(() => new BoardClient({ endpoint, token: 'fixture' }), { message: 'Invalid board endpoint' });
  }
  await assert.rejects(client(() => Response.json({ error: 'secret' }, { status: 403 })).snapshot(), { status: 403, message: 'Board returned HTTP 403' });
  await assert.rejects(client(() => { throw Error('secret'); }).snapshot(), { message: 'Board request failed' });
  await assert.rejects(client(() => new Response('secret')).snapshot(), { message: 'Invalid board JSON' });
});

test('review sends exactly the previewed action, only on approval', async () => {
  for (const approved of [false, true]) {
    const fixture = offlineClient();
    const sent = [];
    let preview;
    const result = await review({ snapshot: () => fixture.snapshot(), action: a => { sent.push(a); return fixture.action(a); } }, async p => {
      preview = JSON.parse(p);
      assert.deepEqual(sent, []);
      return approved;
    });
    assert.equal(preview.channel, 'demo/general');
    assert.deepEqual(sent, approved ? [preview.action] : []);
    assert.deepEqual(result, approved ? { id: 8, cursor: 14 } : null);
  }
});