// The records one module committed, read from the emulated device: boot the patched runtime with the debug module and the module, wait for
// the runtime's `MODULE_COMMIT` event, read the record block it names and decode it with `patchkit records --json`.
//
//   node tools/devtools/commit_records.ts --device dn2|dt2 --module ID --dkm FILE.dkm --patchkit BIN [--binary DIGI_EMUD]
//        [--debug-dkm FILE.dkm] [--emulated-ms 60000]
//
// Prints one JSON object on stdout: { module, latched: [mask per machine], commit: {index, machineRows, paramRows, pageRows, addr, size}, records: {counts, params, ...} }.
// `module:check` (tools/patchkit/pk_modcheck.py) writes `[uses]` and the gallery params from it; conformance reads its cases from it.
// The event's encoding is isolated in `decodeCommitEvent` (packages/test-support/src/commit-records.ts). Not run end to end yet: it needs
// a module and the runtime's `MODULE_COMMIT` event.
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { parseArgs } from 'node:util';
import { DkSession, EmuUsbTransport } from '../../packages/dk-session/src/index.ts';
import { LOG_KIND } from '../../packages/dk-session/src/gen_proto.ts';
import { committedRecords, type RecordsExec } from '../../packages/test-support/src/commit-records.ts';
import { Events } from '../../packages/test-support/src/events.ts';
import { repoRoot, type Device } from '../../packages/test-support/src/inputs.ts';
import { Session } from '../../packages/test-support/src/session.ts';

const FIRMWARE: Record<Device, string> = { dt2: '1.17', dn2: '1.12' };

const { values } = parseArgs({
  options: {
    device: { type: 'string' },
    module: { type: 'string' },
    dkm: { type: 'string' },
    'debug-dkm': { type: 'string' },
    patchkit: { type: 'string' },
    binary: { type: 'string' },
    'emulated-ms': { type: 'string', default: '60000' },
  },
});
const usage =
  'usage: commit_records.ts --device dn2|dt2 --module ID --dkm FILE.dkm --patchkit BIN [--binary DIGI_EMUD] [--debug-dkm FILE.dkm] [--emulated-ms N]';
if ((values.device !== 'dn2' && values.device !== 'dt2') || !values.module || !values.dkm || !values.patchkit) {
  throw new Error(usage);
}
const device: Device = values.device;

/** Runs the patchkit binary BIN with stdin input (this tool is no Nx task, so it spawns the binary itself). */
const patchkitExec =
  (bin: string): RecordsExec =>
  (args, { input }) =>
    new Promise((resolve, reject) => {
      const child = spawn(bin, args, { stdio: ['pipe', 'pipe', 'pipe'] });
      const out: Buffer[] = [];
      const err: Buffer[] = [];
      child.stdout.on('data', (b: Buffer) => out.push(b));
      child.stderr.on('data', (b: Buffer) => err.push(b));
      child.on('error', reject);
      child.on('close', (code) =>
        code === 0
          ? resolve({ stdout: Buffer.concat(out).toString('utf8') })
          : reject(new Error(`${bin} ${args.join(' ')} failed (${code}): ${Buffer.concat(err).toString('utf8')}`)),
      );
      child.stdin.end(input);
    });
const root = repoRoot();
const binary = values.binary ?? process.env[`DIGI_EMUD_${device.toUpperCase()}`];
if (!binary) throw new Error(`no daemon: pass --binary or set DIGI_EMUD_${device.toUpperCase()}`);
const debugDkm = values['debug-dkm'] ?? path.join(root, 'dist/modules', device, 'debug.dkm');
const commitKind = (LOG_KIND as Record<string, number>).MODULE_COMMIT;
if (commitKind === undefined) throw new Error('the runtime has no MODULE_COMMIT log kind yet (S3 package B)');
const samples = device === 'dt2' ? path.join(root, 'out/runs/patchkit/samples') : undefined;
const runDir = fs.mkdtempSync(path.join(root, 'out', `commit-records-${process.pid}-`));

let session: Session | undefined;
try {
  session = await Session.start({
    device,
    firmware: FIRMWARE[device],
    dsp: 'zero',
    binary,
    runDir,
    ...(samples ? { card: samples } : {}),
    runtime: { patchSet: 'emulator' },
  });
  const s = session;
  // a runtime session runs freely: the daemon advances emulated time by itself, so the protocol waits in wall time
  const dk = new DkSession(new EmuUsbTransport((method, params) => s.client.call(method, params)));
  // the first boot: the runtime answers INFO once its protocol is up
  for (const end = Date.now() + 120_000; ; await new Promise((r) => setTimeout(r, 500))) {
    try {
      await dk.info();
      break;
    } catch (e) {
      if (Date.now() > end) throw e;
    }
  }
  const paths: string[] = [];
  for (const file of [debugDkm, values.dkm]) {
    const store = `modules/${path.basename(file)}`;
    await dk.put(store, fs.readFileSync(file));
    paths.push(store);
  }
  await dk.select(paths);
  await dk.restart();
  await dk.waitForBoot();
  const events = new Events(dk, s, { bootStart: LOG_KIND.BOOT_START });
  await events.listen({ fromBoot: true });
  const modules = await dk.modules();
  const index = modules.find((m) => m.name === values.module)?.index;
  if (index === undefined) throw new Error(`${values.module} is not loaded: ${modules.map((m) => m.name).join(', ')}`);
  const event = await events.until(commitKind, (e) => e.args[0] === index, {
    emulatedMs: Number(values['emulated-ms']),
  });
  const result = await committedRecords(event, (addr) => s.client.memPeek(addr), patchkitExec(values.patchkit));
  // the latch masks of the module's machines (the record block has no latched flag): one MODULE_LATCH event per machine that latches,
  // logged before the commit event; `latched[n]` is machine n's mask, bit w for its parameter word w
  const latchKind = (LOG_KIND as Record<string, number>).MODULE_LATCH;
  if (latchKind === undefined) throw new Error('the runtime has no MODULE_LATCH log kind');
  const latched: number[] = Array.from({ length: result.commit.machineRows }, () => 0);
  for (const e of await dk.readLog()) {
    if (e.kind === latchKind && e.args[0] === index && e.args[1]! < latched.length) latched[e.args[1]!] = e.args[2]!;
  }
  console.log(JSON.stringify({ module: values.module, latched, ...result }, null, 2));
} finally {
  await session?.close();
  fs.rmSync(runDir, { recursive: true, force: true });
}
