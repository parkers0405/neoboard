import { offlineClient } from './fixture.ts';

const client = offlineClient();
const snapshot = await client.snapshot();
const changes = await client.events(snapshot.cursor);
console.log(`${snapshot.channels.length} visible channel(s), ${snapshot.messages.length} message(s), ${changes.length} new event(s).`);