import { workerFixture } from './worker-fixture.ts';
import { workOnce } from './work-once.ts';

const { alpha, beta, advance } = workerFixture();
// Both reads see the same ready task. Six seconds per stage means completion
// is past the original ten-second deadline, but inside the renewed lease.
const checkpoint = async () => advance(6);
const results = await Promise.all([workOnce(alpha, checkpoint), workOnce(beta, checkpoint)]);
const [row] = (await alpha.snapshot()).messages;
console.log(`alpha: ${results[0]}; beta: ${results[1]}. Task ${row.id}: ${row.task?.result}.`);