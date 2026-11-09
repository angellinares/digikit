import path from 'node:path';
import { describe, expect, test } from 'vitest';
import type { Device, Session } from '@digi/test-support';
import { bootInfo, E_LOADING } from '@digi/regression/runtime/card-kit.ts';
import { cardRig, RT } from '@digi/regression/runtime/card-rig.ts';
import { advance, pickSine, rpc } from '@digi/regression/runtime/fx-live.ts';
import { tap } from '@digi/regression/runtime/fx-session.ts';
import { FIRMWARE, FULL_TIER } from '@digi/regression/runtime/devices.ts';
import {
  diffSlot,
  dynType,
  panelKey,
  panelTurn,
  pattern0Changes,
  slotImage,
  slotSectors,
  soundRecords,
  SLOT0,
  STAMP,
  trackType,
  typeAt,
} from '@digi/regression/runtime/project-kit.ts';
import { cardAfter } from '@digi/regression/runtime/write-safety.ts';

/**
 * The project round trip across module sets (finding 39; port of tools/patchkit/pk_digikit_project.py `run_roundtrip`).
 * A project with run-time machines (parameter values, a p-lock and a preset lock) is saved by the firmware through the panel, loaded and saved again under the
 * other module sets: the project slot equals the first save but for the header stamp, and the sounds keep their machine types.
 * Every TypeScript daemon is coupled (ColdFire and DSP), so `roundtrip-dt2-coupled` is the same case as `roundtrip-dt2`.
 */

interface Scenario {
  name: string;
  device: Device;
  full: boolean;
  order: string[];
  /** `<module id>:<machine id>` of the sound records the saves must carry. */
  types: string[];
  machines: { track: number; machine: string; hint: number }[];
  sets: Record<string, string[]>;
}

const SETS_DT2 = { A: ['test/dyn-params', 'test/sine'], B: ['test/sine'], A2: ['test/sine', 'test/dyn-params'] };
const DT2_MACHINES = [
  { track: 1, machine: 'test/dyn-params:dynp', hint: 7 },
  { track: 2, machine: 'test/sine:sine', hint: 8 },
];
const DT2_TYPES = ['test/dyn-params:dynp', 'test/sine:sine'];
const LOCK_TURN = 11;
const SOUND_LOCK_TURN = 2; // sine has one parameter (level)
const SCENARIOS: Scenario[] = [
  {
    name: 'roundtrip-dn2',
    device: 'dn2',
    full: true,
    order: ['A', 'A', 'B', 'A2'],
    types: ['test/dyn-dn2:dyn_pluck'],
    machines: [{ track: 1, machine: 'test/dyn-dn2:dyn_pluck', hint: 5 }],
    sets: { A: ['test/dyn-dn2', 'test/sine'], B: ['test/sine'], A2: ['test/sine', 'test/dyn-dn2'] },
  },
  {
    name: 'roundtrip-dt2',
    device: 'dt2',
    full: false,
    order: ['A', 'B'],
    types: DT2_TYPES,
    machines: DT2_MACHINES,
    sets: SETS_DT2,
  },
  {
    name: 'roundtrip-dt2-orders',
    device: 'dt2',
    full: true,
    order: ['A', 'A', 'B', 'A2'],
    types: DT2_TYPES,
    machines: DT2_MACHINES,
    sets: SETS_DT2,
  },
];

const storeName = (id: string) => `modules/${id.split('/').at(-1)}.dkm`;

/** The panel script that makes the project: the run-time machine on track 1 (and the second one on track 2), parameter values, a p-lock and a sound lock on step 1 of track 1. */
async function buildProject(
  session: Session,
  device: Device,
  machines: { track: number; type: number; hint: number }[],
) {
  for (const m of machines) {
    if (m.track !== 1) await tap(session, `TRIG ${m.track}`, 300, 'TRK');
    if (device === 'dt2') {
      // The first SRC list after a project save may not take, so the hint is tried up to four times; every try starts from the top of the list (12 UP presses).
      const seen: [number, number][] = [];
      for (const downs of [m.hint, m.hint, m.hint, m.hint, m.hint + 1, m.hint - 1]) {
        await pickSine(session, downs, 12);
        seen.push([downs, await trackType(session, m.track)]);
        if (seen.at(-1)![1] === m.type) break;
      }
      expect(seen.at(-1)![1], `track ${m.track} holds type ${m.type}; (downs, type) ${JSON.stringify(seen)}`).toBe(
        m.type,
      );
    } else await pickSine(session, m.hint);
    await tap(session, device === 'dt2' ? 'SRC' : 'SYN', 300);
    for (const [enc, by] of [
      ['ENC A', 9],
      ['ENC B', -7],
    ] as const)
      await panelTurn(session, enc, by, 100);
  }
  if (machines.some((m) => m.track !== 1)) await tap(session, 'TRIG 1', 300, 'TRK');
  // a note trig, a p-lock and a preset (sound) lock on it: GRID RECORDING, tap the trig, hold it and turn knobs
  await tap(session, 'RECORD', 200);
  await tap(session, 'TRIG 1', 200);
  await panelKey(session, 'TRIG 1', true);
  await advance(session, 100);
  await panelTurn(session, 'ENC C', LOCK_TURN, 100);
  await panelTurn(session, 'LEVEL', SOUND_LOCK_TURN, 300);
  await panelKey(session, 'TRIG 1', false);
  await advance(session, 300);
  await tap(session, 'RECORD', 200);
}

async function saveProject(session: Session, device: Device) {
  await tap(session, 'SETTINGS', 300, 'FUNC');
  await tap(session, 'YES', 500); // slot 001
  if (device === 'dn2')
    await tap(session, 'YES', 3500); // the name, save
  else await advance(session, 3500);
}

async function loadProject(session: Session) {
  await tap(session, 'SETTINGS', 300);
  for (let i = 0; i < 3; i++) await tap(session, 'YES', 500);
  await tap(session, 'YES', 6000);
  await session.expectNoFault('project loaded');
}

for (const sc of SCENARIOS) {
  describe.skipIf(sc.full && !FULL_TIER)(`${sc.device.toUpperCase()} ${FIRMWARE[sc.device]} ${sc.name}`, () => {
    const { device } = sc;
    const modules = [...new Set(Object.values(sc.sets).flat())];
    const selectionOf = (set: string) => sc.sets[set]!.map(storeName);
    // The first boot runs the DT2 filesystem check; a card the firmware saved a project on never does, so the later boots are a second daemon with the check relaxed
    // (the environment is the daemon's, as in Python where `Ctx.rig` relaxes used cards only). The stock firmware's project seeds the first card (Python `Env.card`).
    const base = { roots: RT, symbols: ['DKRT_ASSIGN'] };
    const first = cardRig(device, sc.name, {
      ...base,
      initial: () => ({ modules, selection: selectionOf(sc.order[0]!) }),
      project: true,
    });
    const later = cardRig(device, `${sc.name}-used`, { ...base, initial: () => ({}), relaxFs: true });

    test('a project passes through the module sets without a change (but for the header stamp)', async () => {
      const known: Record<string, number> = {};
      const saves: { set: string; slots: Map<number, Buffer> }[] = [];
      const afterCards: string[] = [];
      const touched = new Set<number>();
      let baseCardAfter = '';
      let baseSlots = new Map<number, Buffer>();
      let card = '';
      let machines: { track: number; type: number; hint: number }[] = [];
      let types: number[] = [];

      for (const [step, set] of sc.order.entries()) {
        const sel = selectionOf(set);
        const rt =
          step === 0 ? await first.first() : await later.load(`step${step}`, { modules, selection: sel, base: card });
        const { session } = rt;
        const kit = step === 0 ? first : later;
        const label = `boot ${step + 1} (set ${set})`;
        // Python `wait_dsp`: a module's DSP part follows its ColdFire part (result LOADING); step up to 5 s until none is left
        let info = await bootInfo(session, kit.plan);
        for (let i = 0; i < 50 && info.results.includes(E_LOADING); i++) {
          await advance(session, 100);
          info = await bootInfo(session, kit.plan);
        }
        expect(info.loaded, `${label}: every module loaded`).toBe(sel.length);
        expect(info.safe, `${label}: no safe mode`).toBe(0);
        expect(
          info.results.every((r) => r === 0),
          `${label}: results ${info.results}`,
        ).toBe(true);
        // the run-time machines get their number from the boot's assignment table; a machine keeps its number in every later boot that loads its module (D7)
        for (const spec of new Set([...sc.types, ...sc.machines.map((m) => m.machine)])) {
          const [module, machine] = spec.split(':') as [string, string];
          if (!sc.sets[set]!.includes(module)) continue;
          const got = await dynType(session, kit.plan, module, machine);
          if (spec in known) expect(got, `set ${set}: ${spec} keeps type ${known[spec]}`).toBe(known[spec]);
          known[spec] = got;
        }
        card = session.cardPath!;
        const exportOverlay = async (tag: string) => {
          const file = path.join(kit.artifacts, `${sc.name}-${step}-${tag}.overlay.bin`);
          await rpc(session, 'card.overlay', { path: file });
          return file;
        };
        if (step === 0) {
          machines = sc.machines.map((m) => ({ ...m, type: known[m.machine]! }));
          types = sc.types.map((t) => known[t]!);
          await saveProject(session, device);
          const baseOverlay = await exportOverlay('base');
          baseSlots = slotSectors(baseOverlay);
          for (const s of baseSlots.keys()) touched.add(s);
          baseCardAfter = cardAfter(card, baseOverlay, path.join(kit.artifacts, `${sc.name}-base.after`));
          await buildProject(session, device, machines);
        } else await loadProject(session);
        await saveProject(session, device);
        const overlay = await exportOverlay('save');
        const slots = slotSectors(overlay);
        for (const s of slots.keys()) touched.add(s);
        const after = cardAfter(card, overlay, path.join(kit.artifacts, `${sc.name}-${step}.after`));
        afterCards.push(after);
        saves.push({ set, slots });
        card = after;
      }

      // compare: the slot of every save against the first
      const imgs = afterCards.map((c) => slotImage(c, touched));
      const baseImg = slotImage(baseCardAfter, touched);
      const d0 = diffSlot(baseImg, imgs[0]!, device);
      expect(d0.equal, `control: the panel script changed the project ${JSON.stringify(d0)}`).toBe(false);
      expect(
        d0.differBytes.pattern ?? 0,
        'control: the first save differs from a save of the untouched project in the patterns',
      ).toBeGreaterThan(0);
      // the firmware saves only the patterns and kits that changed: an untouched project's save does not write pattern 1 at all, so its bytes compare against the card's zeros
      const pc = pattern0Changes(baseImg, imgs[0]!);
      expect(
        pc.filter((o) => o[0] < 0x20).length,
        'control: pattern 1 gained a trig (its first bytes)',
      ).toBeGreaterThan(0);
      expect(
        pc.filter((o) => o[0] >= 0x20).length,
        'control: pattern 1 gained lock data (the bytes after the trig table)',
      ).toBeGreaterThan(0);
      expect(
        (d0.differBytes.kit ?? 0) + (d0.differBytes.pool ?? 0),
        'control: the sounds changed too (parameter values, the machine type)',
      ).toBeGreaterThan(0);
      const typesIn = (img: Map<number, Buffer>) => soundRecords(img, typeAt(device)).map(([, ty]) => ty);
      for (const want of types)
        expect(typesIn(imgs[0]!), `the first save holds a sound with the run-time machine's type ${want}`).toContain(
          want,
        );
      for (let k = 1; k < saves.length; k++) {
        const d = diffSlot(imgs[0]!, imgs[k]!, device, STAMP);
        expect(
          d.equal,
          `project slot after set ${saves[k]!.set} (boot ${k + 1}) equals the first save (but for the header stamp) ${JSON.stringify(d)}`,
        ).toBe(true);
        if (k >= 2) {
          const d2 = diffSlot(imgs[1]!, imgs[k]!, device);
          expect(
            d2.equal,
            `project slot after set ${saves[k]!.set} (boot ${k + 1}) is byte-identical to the second save (stamp included) ${JSON.stringify(d2)}`,
          ).toBe(true);
        }
        for (const want of types)
          expect(
            typesIn(imgs[k]!),
            `boot ${k + 1} (set ${saves[k]!.set}): sound records still carry type ${want}`,
          ).toContain(want);
      }
      const stamps = imgs.map((img) => img.get(SLOT0)!.readUInt32BE(0x18));
      expect(
        stamps.length < 3 || (stamps[0] !== stamps[1] && new Set(stamps.slice(1)).size === 1),
        `the header stamp changes once (first save to second) and then stays: ${stamps.map((x) => x.toString(16))}`,
      ).toBe(true);
    }, 3_600_000);
  });
}
