import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import type { DkModule } from '@digi/dk-session';
import type { Device } from '@digi/test-support';
import { afterAll, describe, expect, test } from 'vitest';
import { record } from './artifacts.ts';
import { bootInfo } from './card-kit.ts';
import { CARD_SYMBOLS, cardRig, RT, type CardRig, type CardSpec } from './card-rig.ts';
import { CARD_TIMEOUT, pausedProtocol } from './debug-kit.ts';
import { FIRMWARE, FULL_TIER, selectDevices, skipEmptyHalf } from './devices.ts';
import { advance } from './fx-live.ts';
import {
  arenaBounds,
  bytesChanged,
  describeHits,
  driveAuditUi,
  guardHits,
  guardPlan,
  loadWatchEnv,
  moduleBlocks,
  type GuardPlan,
} from './memaudit-kit.ts';
import { needsOf } from './dkm.ts';
import { dynType, type PanelMachine } from './project-kit.ts';
import { dmaHoles } from './write-safety.ts';
import { DSP_GRANULE, fullSet, geometryOf, LOADED, packSynth, usageOf, type Want } from './synth.ts';

/**
 * The ColdFire memory audit as a device test (port of the `memaudit` part of pk_digikit_memaudit.py; protected ranges and allowed writers: memaudit-kit.ts).
 * Per device and module set, two daemons: the first loads the set on a ColdFire-only rig, arms the write guard after the boot and drives the UI
 * (DIGIKIT menu, MODULES, machine on a track, p-lock, project save, edit, save, load); the second boots the same card cold with the write watch on the
 * same ranges from the first cache enable, so the load of the modules and their `dkm_init` are audited too.
 */

export interface AuditMachine {
  track: number;
  module: string;
  machine: string;
  /** Position in the machine list (the run-time machines follow the stock ones). */
  hint: number;
}

export interface AuditSet {
  title: string;
  /** Module ids in load order. */
  modules: string[];
  /** Modules whose block must change while the guard is armed (they ran). */
  mustRun?: string[];
  machines?: AuditMachine[];
  /** Machine list entries chosen on the track and turned (`module:audit`). */
  sweep?: number;
  /** Negative control: the one store the audit must report, by this module's code, in the arena. */
  stray?: string;
  /** Behind the modules, the full set of the capacity tests (synth.ts `fullSet`): every limit used, three modules refused. */
  full?: boolean;
}

export interface AuditOptions {
  devices?: readonly Device[];
  /** Full tier only. */
  full?: boolean;
  /** The packed module as a card store entry (default: the prepared artifact of `rig.mod`). May skip the case by returning a reason. */
  artifact?: (rig: CardRig, id: string) => Promise<{ name: string; file: string } | { skip: string }>;
}

const E_LOADING = 1;

/** The `t: "w"` events (CPU stores) of the write-watch logs a daemon has written so far. */
function loggedStores(dir: string): { pc: number; addr: number; size: number }[] {
  const out: { pc: number; addr: number; size: number }[] = [];
  const files = fs.readdirSync(dir).filter((f) => /^writewatch(-\d+)?\.jsonl$/.test(f));
  if (!files.length) throw new Error(`no write-watch log in ${dir}: the load pass would prove nothing`);
  for (const file of files) {
    for (const line of fs.readFileSync(path.join(dir, file), 'utf8').split('\n').filter(Boolean)) {
      const e = JSON.parse(line) as { t?: string; pc: string; addr: string; size: number };
      if (e.t === 'w') out.push({ pc: Number.parseInt(e.pc, 16), addr: Number.parseInt(e.addr, 16), size: e.size });
    }
  }
  return out;
}

export function defineMemaudit(set: (device: Device) => AuditSet | undefined, o: AuditOptions = {}): void {
  const selected = selectDevices(o.devices);
  skipEmptyHalf(selected, 'memaudit');
  for (const device of selected) {
    const spec = set(device);
    if (!spec) continue;
    describe.skipIf(o.full === true && !FULL_TIER)(
      `${device.toUpperCase()} ${FIRMWARE[device]}: memaudit ${spec.title}`,
      () => {
        let guard: GuardPlan | undefined;
        let blocks: DkModule[] = [];
        let cardSpec: CardSpec | undefined;
        const synthDir = spec.full ? fs.mkdtempSync(path.join(os.tmpdir(), 'digi-memaudit-synth-')) : '';
        afterAll(() => {
          if (synthDir) fs.rmSync(synthDir, { recursive: true, force: true });
        });
        const base = {
          roots: [...RT, 'core/digikit-menu'],
          symbols: [...CARD_SYMBOLS, 'DKRT_ASSIGN', 'DKRT_ARENA', '_dl_area'],
          dsp: 'zero' as const,
        };
        // a negative control's guard hit is a fault event of the session: the case end must not fail on it (the test judges the faults itself)
        const common = { ...base, project: true, uploads: true, relaxFs: true, faults: spec.stray !== undefined };

        describe('drive', () => {
          const h = cardRig(device, `memaudit-${spec.title}`, {
            ...common,
            // the daemon's own watch has nothing to judge here (the guard does): it keeps only the runtime's boot identity
            env: (prepared) => {
              const boot = prepared.plan.symbols;
              const key = Object.keys(boot).find((k) => k.endsWith(':DKRT_BOOT'))!;
              return { DIGI_WRITE_WATCH: `0x${boot[key]!.toString(16)}-0x${(boot[key]! + 8).toString(16)}` };
            },
          });

          test(
            'the set loads; while the UI is driven no store lands outside the module blocks and the patch code',
            async (ctx) => {
              const mods = [];
              for (const id of spec.modules) {
                const m = o.artifact ? await o.artifact(h, id) : h.mod(id);
                if ('skip' in m) return ctx.skip(m.skip);
                mods.push(m);
              }
              const wants: Want[] = mods.map(() => LOADED);
              const loaded = [...spec.modules]; // the module ids MODULES must list
              if (spec.full) {
                // the second daemon builds the same card: the files must outlive the first daemon's artifact directory
                const g = geometryOf(device, h.plan);
                const fill = fullSet(g, (await needsOf(mods.map((m) => m.file))).map(usageOf));
                for (const [i, f] of fill.entries()) {
                  mods.push({
                    name: `modules/full${i}.dkm`,
                    file: await packSynth(device, f.spec, synthDir, `${i}.dkm`),
                  });
                  wants.push(f.want);
                  if (f.want === LOADED) loaded.push(f.spec.name);
                }
              }
              cardSpec = { modules: mods, selection: mods.map((m) => m.name) };
              const runtime = await h.load('audit', cardSpec);
              const { session } = runtime;

              const info = await bootInfo(session, h.plan);
              expect(info.safe, 'not in safe mode').toBe(0);
              expect(info.count, 'every module of the set is in the boot record').toBe(mods.length);
              expect(
                info.results.every((r, i) => (wants[i] === LOADED ? r === 0 || r === E_LOADING : r === wants[i])),
                `every module loaded (ColdFire part), the refused ones with their code: ${JSON.stringify(info.results)} for ${JSON.stringify(wants)}`,
              ).toBe(true);

              const { modules, cap } = await pausedProtocol(runtime, async (dk) => ({
                modules: await dk.modules(),
                cap: await dk.capacity(),
              }));
              blocks = modules;
              const arena = await arenaBounds(session, h.plan);
              expect(modules.map((m) => m.name).sort(), 'MODULES lists the loaded modules').toEqual([...loaded].sort());
              for (const m of modules)
                expect(
                  arena[0] <= m.cf_base && m.cf_base + m.cf_size <= arena[1],
                  `${m.name}: the block lies inside the arena ${arena.map((a) => a.toString(16))}`,
                ).toBe(true);

              if (spec.full)
                expect(
                  [cap.ARENA!.limit - cap.ARENA!.used, cap.ARENA!.largest, cap.DSP_AREA!.largest < DSP_GRANULE],
                  'the full set uses the arena to its last byte and the DSP area to its last whole granule',
                ).toEqual([0, 0, true]);

              const plan = guardPlan(device, FIRMWARE[device], h.plan, modules, arena);
              guard = plan;
              record(h.artifacts, 'memaudit-plan', {
                protect: plan.protect.map((r) => [r.label, r.lo, r.hi]),
                code: plan.code.map((c) => [c.owner, c.lo, c.hi]),
                modules: modules.map((m) => [m.name, m.cf_base, m.cf_size]),
              });

              const machines: PanelMachine[] = [];
              for (const m of spec.machines ?? [])
                machines.push({
                  track: m.track,
                  type: await dynType(session, h.plan, m.module, m.machine),
                  hint: m.hint,
                });

              const before = await moduleBlocks(session, modules);
              await session.client.guardSet({
                protect: plan.protect.map(({ lo, hi, label }) => ({ lo, hi, label })),
                allow_pc: plan.code.map(({ lo, hi }) => ({ lo, hi })),
              });
              try {
                await driveAuditUi(session, machines, spec.sweep, !spec.stray);
                const { total, hits } = await guardHits(session, modules, plan.code);
                const after = await moduleBlocks(session, modules);
                const changed = Object.fromEntries(
                  modules.map((m) => [m.name, bytesChanged(before.get(m.name)!, after.get(m.name)!)]),
                );
                record(h.artifacts, 'memaudit-result', { total, hits, changed });
                for (const name of spec.mustRun ?? [])
                  expect(
                    changed[name],
                    `${name} ran while the guard was armed (its block changed): ${JSON.stringify(changed)}`,
                  ).toBeGreaterThan(0);
                if (spec.stray) {
                  const others = (await session.faults()).filter((e) => !e.kind.startsWith('guard'));
                  expect(others, 'no fault but the expected guard hit').toEqual([]);
                  expect(
                    { total, hits: hits.map((x) => [x.writer, x.label]) },
                    `exactly one store, by the code of ${spec.stray}, inside the protected arena: ${describeHits(hits)}`,
                  ).toEqual({ total: 1, hits: [[`module ${spec.stray}`, 'arena']] });
                } else
                  expect(
                    total,
                    `no store outside the module blocks and the patch code while the UI was driven: ${describeHits(hits)}`,
                  ).toBe(0);
              } finally {
                await session.client.guardClear(); // the case's own guard check must not see the negative control's store
              }
            },
            CARD_TIMEOUT,
          );
        });

        describe('load pass', () => {
          const h = cardRig(device, `memaudit-load-${spec.title}`, {
            ...common,
            // the watch of this daemon is the audit's: the ranges come from the first pass (the arena is deterministic)
            env: (prepared) => {
              if (!guard) return {}; // the drive test failed: the load pass skips
              return loadWatchEnv(guard, dmaHoles(prepared.plan));
            },
          });

          test(
            'a cold boot of the same card with the write watch on: the load and init of the modules store nothing outside their blocks',
            async (ctx) => {
              if (!cardSpec || !guard) return ctx.skip('the drive test did not get as far as arming the guard');
              const runtime = await h.load('load-pass', cardSpec);
              await advance(runtime.session, 300);
              const stores = loggedStores(h.artifacts);
              expect(
                stores.map((s) => `pc 0x${s.pc.toString(16)} addr 0x${s.addr.toString(16)} size ${s.size}`),
                `no store while the modules loaded (${blocks.map((m) => m.name)})`,
              ).toEqual([]);
            },
            CARD_TIMEOUT,
          );
        });
      },
    );
  }
}
