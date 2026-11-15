import { chmodSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { openRig, type RigSetup } from '../src/rig.ts';

// A stand-in for `sharc-rig --serve`: one answer line per input line. A setup with `"fail": true` is refused, an `ops` line with a
// call to pc 666 is an error, a call to pc 777 ends the process, other calls echo their pc as `instructions`.
const FAKE = `#!/usr/bin/env node
const rl = require('node:readline').createInterface({ input: process.stdin });
let up = false;
rl.on('line', (line) => {
  const j = JSON.parse(line);
  if (!up) {
    if (j.fail) return void process.stdout.write(JSON.stringify({ error: 'refused' }) + '\\n');
    up = true;
    return void process.stdout.write(JSON.stringify({ build: 'fake', results: [] }) + '\\n');
  }
  const pcs = j.ops.filter((o) => o.call).map((o) => o.call.pc);
  if (pcs.includes(666)) return void process.stdout.write(JSON.stringify({ error: 'bad call' }) + '\\n');
  if (pcs.includes(777)) process.exit(3);
  process.stdout.write(JSON.stringify({ build: 'fake', results: pcs.map((pc) => ({ instructions: pc })) }) + '\\n');
});
`;

let dir: string;
let file: string;
beforeAll(() => {
  dir = mkdtempSync(path.join(tmpdir(), 'rig-fake-'));
  file = path.join(dir, 'fake-rig.cjs');
  writeFileSync(file, FAKE);
  chmodSync(file, 0o755);
  process.env.DIGI_SHARC_RIG = file;
});
afterAll(() => {
  delete process.env.DIGI_SHARC_RIG;
  rmSync(dir, { recursive: true });
});

const setup: RigSetup = { return_sw: 1, breaks: [], code: [] };
const call = (pc: number) => ({ call: { pc, regs: {}, max_steps: 1, reads: [], read_regs: [] } });

describe('openRig', () => {
  it('answers each batch of ops with the results of its calls, in order', async () => {
    const { port } = await openRig(setup);
    expect((await port.run([call(5), { poke: [[1, 2]] }, call(6)])).map((r) => r.instructions)).toEqual([5, 6]);
    expect((await port.run([call(7)])).map((r) => r.instructions)).toEqual([7]);
    await port.close();
  });

  it('rejects a batch the rig refuses and stays usable', async () => {
    const { port } = await openRig(setup);
    await expect(port.run([call(666)])).rejects.toThrow('bad call');
    expect((await port.run([call(8)])).map((r) => r.instructions)).toEqual([8]);
    await port.close();
  });

  it('rejects a setup the rig refuses', async () => {
    await expect(openRig({ ...setup, fail: true } as RigSetup)).rejects.toThrow('refused');
  });

  it('rejects a batch when the process ends, with its exit code', async () => {
    const { port } = await openRig(setup);
    await expect(port.run([call(777)])).rejects.toThrow('exited with 3');
    await expect(port.run([call(1)])).rejects.toThrow('exited with 3');
    await port.close();
  });
});
