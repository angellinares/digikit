import fs from 'node:fs';
import path from 'node:path';
import { hexSha256, type DkLogEvent } from '@digi/dk-session';
import { LOG_KIND, SERVICE_CMD } from '@digi/dk-session/gen_proto';
import { expect } from 'vitest';
import type { PatchedSession } from '../support.ts';
import {
  DEBUG_MODULE,
  expectRuntimeReady,
  installWithDebug,
  liveArtifacts,
  record,
  waitEmulated,
} from '../runtime/live.ts';

export interface RuntimeModule {
  id: string;
  /** SDK contract slot the module fills: 1 machine, 2 effect, 3 service. */
  contract: 1 | 2 | 3;
  /** Whether the module carries a DSP image (a dynamic machine or effect). */
  dsp: boolean;
  type: 'machine' | 'fx' | 'service';
}

/** The `[module]` table of a module.toml as flat key -> raw value text (only the keys read below). */
function moduleTable(text: string): Map<string, string> {
  const out = new Map<string, string>();
  let inModule = false;
  for (const line of text.split('\n')) {
    const header = /^\s*\[+([^\]]+)\]+/.exec(line);
    if (header) {
      inModule = header[1] === 'module';
      continue;
    }
    const pair = inModule ? /^(\w+)\s*=\s*(.*?)\s*(?:#.*)?$/.exec(line) : null;
    if (pair) out.set(pair[1]!, pair[2]!);
  }
  return out;
}

/** Production upload candidates: the shipped modules (`modules/<kind>/<name>`) that are `dynamic`, a service or a
 * `host` module. Same rule as RUNTIME_UPLOADS in tools/devtools/verify_route.py. Framework/static modules are
 * firmware prerequisites. */
export function scanRuntimeModules(root = path.join(import.meta.dirname, '../../../modules')): RuntimeModule[] {
  const found: RuntimeModule[] = [];
  for (const kind of fs.readdirSync(root).sort()) {
    const kindDir = path.join(root, kind);
    if (!fs.statSync(kindDir).isDirectory()) continue;
    for (const name of fs.readdirSync(kindDir).sort()) {
      const manifest = path.join(kindDir, name, 'module.toml');
      if (!fs.existsSync(manifest)) continue;
      const table = moduleTable(fs.readFileSync(manifest, 'utf8'));
      const type = table.get('type')?.replaceAll('"', '');
      const dynamic = table.get('dynamic') === 'true';
      if (!(dynamic || table.get('host') === 'true' || type === 'service')) continue;
      if (type !== 'machine' && type !== 'fx' && type !== 'service') continue;
      found.push({
        id: table.get('id')!.replaceAll('"', ''),
        type,
        contract: type === 'machine' ? 1 : type === 'fx' ? 2 : 3,
        dsp: dynamic && type !== 'service',
      });
    }
  }
  return found;
}

export const runtimeModules: readonly RuntimeModule[] = scanRuntimeModules();

/** The module whose id ends in the file's base name: `tests/regression/modules/<leaf>.test.ts` tests `<author>/<leaf>`. */
export function runtimeModuleOf(testFileUrl: string): RuntimeModule {
  const leaf = path.basename(new URL(testFileUrl).pathname, '.test.ts');
  const module = runtimeModules.find((candidate) => candidate.id.split('/')[1] === leaf);
  if (!module) throw new Error(`no run-time module named ${leaf} under modules/`);
  return module;
}

/** Real USB upload, persisted bytes, selected reboot, registration and fault acceptance.
 * DSP allocation/registration does not establish rendered audio correctness. */
export async function expectModuleLoad(
  runtime: PatchedSession,
  module: RuntimeModule,
  artifact: string,
): Promise<void> {
  const { session, dk } = runtime;
  const bytes = fs.readFileSync(artifact);
  const storePath = `modules/${module.id.split('/')[1]}.dkm`;
  const hash = await hexSha256(bytes);
  const artifacts = path.join(liveArtifacts(session, 'module-load'), module.id.split('/')[1]!);
  fs.mkdirSync(artifacts, { recursive: true });

  await expectRuntimeReady(session);
  expect(await dk.list(), 'fresh daemon has an empty store').toEqual([]);
  expect(await dk.modules(), 'fresh runtime has no uploaded modules').toEqual([]);
  if (module.type === 'service') {
    expect((await dk.request(SERVICE_CMD.REMOTE_STATE)).status, 'remote service is absent before upload').toBe(-60);
  }
  const epoch = (await session.status()).epoch;
  if (typeof epoch !== 'number') throw new Error('daemon status must report a numeric epoch');
  // the debug module is selected first: the log (LOG) exists only while it is loaded, and this cell reads it
  await installWithDebug(runtime, module.id);
  expect(
    (await dk.modules()).map((entry) => entry.name),
    'only the uploaded module and the debug module are registered',
  ).toEqual([DEBUG_MODULE, module.id]);
  expect((await session.status()).epoch, 'protocol restart boots another epoch').toBeGreaterThan(epoch);
  const entries = await dk.list();
  record(artifacts, 'store', entries);
  expect(entries.find((entry) => entry.path === storePath)).toEqual({
    path: storePath,
    size: bytes.length,
    sha256: hash,
  });
  expect(entries.map((entry) => entry.path)).toEqual(['modules/debug.dkm', storePath].sort());
  expect(await dk.verifyFile(storePath, bytes), 'module bytes survive reboot').toBe(true);

  try {
    if (module.dsp) {
      // MOD_LOAD publishes the CF module before its DSP part completes over UI-loop passes.
      // This cell selects the debug module (index 0) and then this module's path, so boot's selection index is one (not MODULES' registry index).
      let completionLog: DkLogEvent[] = [];
      try {
        await expect
          .poll(
            async () => {
              completionLog = await dk.readLog();
              return completionLog.some((event) => event.kind === LOG_KIND.MOD_DSP && event.args[0] === 1);
            },
            { timeout: 30_000, interval: 500, message: 'selected module DSP completion' },
          )
          .toBe(true);
      } finally {
        record(artifacts, 'dsp-completion-log', completionLog);
      }
      expect(
        completionLog
          .filter((event) => event.kind === LOG_KIND.MOD_DSP && event.args[0] === 1)
          .map((event) => event.args[1]),
        'DSP loader completes the selected module successfully',
      ).toEqual([0]);
    }
    const loaded = (await dk.modules()).find((entry) => entry.name === module.id)!;
    expect(loaded.version, 'loader reports the prepared module version').toBe(bytes.readUInt32BE(0x34));
    const contracts = Array.from({ length: 4 }, (_, index) => [bytes[0x38 + 2 * index], bytes[0x39 + 2 * index]]);
    expect(loaded.contracts, 'loader reports the prepared SDK contracts').toEqual(contracts);
    expect(loaded.contracts[module.contract]![0], 'intended module contract is active').toBeGreaterThan(0);
    if (module.dsp) {
      expect(loaded.dsp_len, 'module DSP image was allocated').toBeGreaterThan(0);
      if (module.type === 'machine') {
        expect(loaded.rows, 'machine has a published DSP voice row').toBeGreaterThan(0);
        expect(loaded.types, 'machine has a published firmware type').toHaveLength(1);
      }
    } else {
      expect(loaded.dsp_len).toBe(0);
      expect(await dk.remoteState(), 'loaded remote service dispatches STATE').toEqual({
        enabled: true,
        format: 1,
        turnLimit: 30,
      });
      await dk.remoteRelease();
    }
    // Allow the loaded registration to run on the coupled DSP before checking its guard counters.
    await waitEmulated(session, 2000);
    const modules = await dk.modules();
    record(artifacts, 'modules', modules);
    expect(modules.map(({ name, faults, guard_off }) => ({ name, faults, guard_off }))).toEqual([
      { name: DEBUG_MODULE, faults: 0, guard_off: false },
      { name: module.id, faults: 0, guard_off: false },
    ]);
    await expect
      .poll(async () => (await dk.readLog()).some((event) => event.kind === LOG_KIND.UI_FIRST), {
        timeout: 30_000,
        interval: 500,
      })
      .toBe(true);
    const log = await dk.readLog();
    record(artifacts, 'boot-log', log);
    expect(
      log.some((event) => event.kind === LOG_KIND.UI_FIRST),
      'selected module boot reaches the firmware UI',
    ).toBe(true);
    const failures = new Set<number>([
      LOG_KIND.SAFE,
      LOG_KIND.DSP_STOPPED,
      LOG_KIND.CRASH_FOUND,
      LOG_KIND.DSP_ABSENT,
      LOG_KIND.MACHINE_FAULT,
      LOG_KIND.MACHINE_DISABLED,
      LOG_KIND.TABLE_BAD,
    ]);
    expect(
      log.filter((event) => failures.has(event.kind)),
      'no runtime crash, missing DSP or disabled registration',
    ).toEqual([]);
    await session.expectNoFault(`loaded ${module.id}`);
  } finally {
    record(artifacts, 'final-modules', await dk.modules());
    record(artifacts, 'final-status', await session.status());
  }
}
