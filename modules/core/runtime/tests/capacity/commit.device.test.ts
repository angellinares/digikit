import { createHash } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { PROVIDER_IDX } from '@digi/dk-session';
import { RESOURCE } from '@digi/dk-session/gen_proto';
import { committedRecords, decodeCommitEvent } from '@digi/test-support/commit-records';
import { expect, test } from 'vitest';
import { cardRigs, type CardRig } from '@digi/regression/runtime/card-rig.ts';
import { dspBytes, word } from '@digi/regression/runtime/fx-live.ts';
import { moduleArtifact } from '@digi/regression/runtime/install.ts';
import { patchkit } from '@digi/regression/runtime/patchkit.ts';
import { dynType } from '@digi/regression/runtime/project-kit.ts';
import { moduleRingEvents, ringEvents } from '@digi/regression/runtime/runtime-menu.ts';
import { planSymbol } from '@digi/regression/runtime/runtime-plan.ts';
import { block, LOADED } from '@digi/regression/runtime/synth.ts';
import { bootEntries, COUPLED_ROOTS, SYMBOLS, type Entry } from './capacity-helpers.ts';

/**
 * The commit of a module (docs/plans/sdk.md 6.3), end to end on both devices, with the fixture test/commit-demo: a machine `cmach` with three
 * params (Mode is latched, Go has a click action) and an effect `cfx` with one page, declared by builder calls in `init`.
 *
 * - the MODULE_COMMIT event gives the rows the module uses and the committed record block; the block holds the params and the effect page;
 * - the machine's voice row in the DSP has the knob ranges and the latch mask of the params;
 * - a module that declares more rows than its `[uses]`, an id that is not in its keys table, or an effect id that another loaded module has, is refused with
 *   its own status and changes nothing: no row, no DSP range, no arena block (the refusals are made from the packed fixture by patching its bytes).
 */
const ID = 'test/commit-demo';
const E_DECL_ID = -27;
const E_DECL_USES = -28;
const E_FX_ID_DUP = -29;
/** Header fields of a .dkm (docs/design/digikit-dkm.md). */
const H_NAME = 0x40;
const H_KEYS_OFF = 0x90;
const H_KEYS_SIZE = 0x94;
const H_DSP_SIZE = 0x9c;
const ROW_BYTES = 96;
const ROW_RANGE0 = 2;
const ROW_LATCH = 22;
/** The offset of the count of resource `id` in the USES entry (tag 2) of the header table (packages/dkneeds/src/keys.rs). */
function usesCountAt(f: Buffer, id: number): number {
  const table = f.readUInt32BE(H_KEYS_OFF);
  let at = table + 8;
  for (let i = f.readUInt16BE(table + 6); i > 0; i--) {
    const count = f.readUInt16BE(at + 2);
    if (f.readUInt16BE(at) === 2) {
      for (let k = 0; k < count; k++) if (f.readUInt32BE(at + 8 + 8 * k) === id) return at + 8 + 8 * k + 4;
    }
    at += 8 + f.readUInt32BE(at + 4);
  }
  throw new Error(`no uses entry for resource ${id}`);
}

/** The machine key the packer writes for the fixture's `test/commit-demo:cmach` (the keys table and the DSP voice record; init passes the index 0). */
const KEY = Buffer.from([0xa6, 0x51, 0x3f, 0x00]);
const KEY2 = Buffer.from([0xa6, 0x51, 0x3e, 0x00]);
/** Knob position of each param (`[adapter.machine] param_slots` of the device file), the sound-array slot of knob 0 and the stock row of the default glyph. */
const DEVICE = {
  dt2: { positions: [1, 2, 4], slotBase: 0, encoder: 0xcc, pageRows: 0 },
  dn2: { positions: [1, 2, 4], slotBase: 25, encoder: 0xd5, pageRows: 1 },
} as const;

/** The file with its sha256 made right again, so that only the patched field is wrong. */
function rehash(file: Buffer): Buffer {
  file.fill(0, 0x10, 0x30);
  createHash('sha256').update(file).digest().copy(file, 0x10);
  return file;
}

function replaceAll(buf: Buffer, from: Buffer, to: Buffer, lo = 0, hi = buf.length): number {
  let n = 0;
  for (let at = buf.indexOf(from, lo); at >= 0 && at + from.length <= hi; at = buf.indexOf(from, at + 1)) {
    to.copy(buf, at);
    n++;
  }
  return n;
}

/** What the good boot of the fixture holds in the arena: all of it, and the part the providers (services the module needs) hold. */
interface GoodArena {
  used: number;
  providers: number;
}
const goodArena = new Map<string, GoodArena>();

/**
 * Boot the fixture alone and record its arena: `providers` is `ARENA.used` less the module's image block and its committed record block (the Collect
 * of its load is freed). The runtime does not publish the providers' own blocks.
 */
async function bootGood(rig: CardRig): Promise<Awaited<ReturnType<typeof bootEntries>>> {
  const entries: Entry[] = [{ name: ID, file: moduleArtifact(rig.device, ID), want: LOADED }];
  const got = await bootEntries(rig, 'commit-good', entries);
  const [event] = await moduleRingEvents(got.rt.session, rig.plan, 'MODULE_COMMIT');
  const used = got.cap.ARENA!.used;
  const providers = used - block(got.mods[0]!.cf_size) - block(decodeCommitEvent(event!).size);
  goodArena.set(rig.device, { used, providers });
  return got;
}

/** The arena of the good boot (a boot of its own, made before the boots under test). */
async function goodArenaOf(rig: CardRig): Promise<GoodArena> {
  if (!goodArena.has(rig.device)) await bootGood(rig);
  return goodArena.get(rig.device)!;
}

/**
 * Refused and left nothing behind: no module listed, no row, no DSP range, no arena block. `good` is `goodArenaOf` of the rig, read before this boot. With no
 * module loaded the arena holds the providers' blocks only, and none when the refused file needed no service (no provider loaded on this boot).
 */
async function expectNothing(
  rig: CardRig,
  got: Awaited<ReturnType<typeof bootEntries>>,
  loaded: string[],
  rows: number,
  good: GoodArena,
) {
  const { mods, cap, rt } = got;
  const providersLoaded = (await ringEvents(rt.session, rig.plan, 'MOD_LOAD')).filter(
    (e) => e.args[0] === PROVIDER_IDX && e.args[1] === 0,
  ).length;
  const providers = providersLoaded ? good.providers : 0;
  expect(
    mods.map((m) => m.name),
    'MODULES lists only the modules that loaded',
  ).toEqual(loaded);
  expect(
    [cap.MACHINE_ROWS!.used, cap.PARAM_ROWS!.used, cap.VOICE_ROWS!.used],
    'the refused module wrote no machine, parameter or voice row',
  ).toEqual([rows, rows * 3, rows]);
  expect(cap.DSP_AREA!.used > 0, 'and holds no DSP range unless an earlier module does').toBe(rows > 0);
  expect(cap.ARENA!.used, 'and no arena block beyond the providers and the modules that loaded').toBe(
    rows > 0 ? good.used : providers,
  );
  expect((await word(rt.session, planSymbol(rig.plan, 'DKRT_TABLE_BAD'))) >>> 24, 'the firmware tables are intact').toBe(0);
  expect(await moduleRingEvents(rt.session, rig.plan, 'MODULE_COMMIT'), 'a refused commit logs no MODULE_COMMIT').toHaveLength(rows);
}

cardRigs(
  'commit of a module',
  {
    dsp: 'sharc',
    roots: COUPLED_ROOTS,
    hold: true,
    symbols: [...SYMBOLS, 'voices.rows', 'dyndsp_info', 'DKRT_ASSIGN'],
  },
  (rig) => {
    const fixture = () => fs.readFileSync(moduleArtifact(rig.device, ID));
    const patched = (name: string, edit: (f: Buffer) => void): string => {
      const f = fixture();
      edit(f);
      const file = path.join(rig.artifacts, name);
      fs.mkdirSync(rig.artifacts, { recursive: true });
      fs.writeFileSync(file, rehash(f));
      return file;
    };

    test('the commit logs its rows, writes the voice row ranges and latch mask and commits the effect page', async () => {
      await rig.first();
      const dev = DEVICE[rig.device];
      const got = await bootGood(rig);
      expect(got.mods.map((m) => m.name)).toEqual([ID]);
      const { session } = got.rt;

      const [event, ...more] = await moduleRingEvents(session, rig.plan, 'MODULE_COMMIT');
      expect(event, 'a MODULE_COMMIT event').toBeDefined();
      expect(more, 'one commit').toEqual([]);
      const { commit, records } = await committedRecords(event!, (a) => session.client.memPeek(a), patchkit);
      expect(commit.index, 'the selection index of the module').toBe(0);
      expect([commit.machineRows, commit.paramRows, commit.pageRows], 'the rows the declarations use').toEqual([
        1,
        3,
        dev.pageRows,
      ]);

      // the committed records: three machine params on the device's knob positions with the stock row of the default glyph, and the effect page
      expect(
        records.params.map((p) => [p.name, p.short, p.slot, p.template, p.min, p.max, p.default]),
        'the params of the machine',
      ).toEqual([
        ['Tone', 'TONE', dev.slotBase + dev.positions[0], dev.encoder, 0, 127 * 256, 64 * 256],
        ['Mode', 'MODE', dev.slotBase + dev.positions[1], dev.encoder, 0, 3 * 256, 0],
        ['Go', 'GO', dev.slotBase + dev.positions[2], dev.encoder, 0, 127 * 256, 0],
      ]);
      expect(records.effects, 'one effect page').toHaveLength(1);
      const page = records.effects[0] as { tab: string; knobs: Record<string, unknown>[] };
      expect(page.tab, 'the short name is the tab label').toBe('CFX');
      expect(page.knobs, 'one knob: Gain 0 to 1.27, default 0.5').toHaveLength(1);
      expect(page.knobs[0]).toMatchObject({ name: 'Gain', short: 'GAIN', min: 0, default: 0.5 });
      expect((page.knobs[0] as { max: number }).max).toBeCloseTo(1.27, 5);

      // the voice row of the machine: the knob ranges of its params and the latch mask (bit 1: the word of Mode)
      const rows = planSymbol(rig.plan, 'voices.rows');
      const first = await word(session, planSymbol(rig.plan, 'dyndsp_info') + 4 * 6);
      const type = await dynType(session, rig.plan, ID, 'cmach');
      let row: number[] | undefined;
      for (let k = 0; k < 8 && !row; k++) {
        const raw = await dspBytes(session, rows + ROW_BYTES * (first + k), ROW_BYTES);
        const words = Array.from({ length: ROW_BYTES / 4 }, (_, i) => raw.readUInt32LE(4 * i));
        if (words[0] === type) row = words;
      }
      expect(row, `a voice row of type ${type}`).toBeDefined();
      const ranges = Array.from({ length: 8 }, (_, j) => row![ROW_RANGE0 + j]);
      const want = Array(8).fill(0);
      want[dev.positions[0]] = (127 * 256 * 65536) >>> 0;
      want[dev.positions[1]] = (3 * 256 * 65536) >>> 0;
      want[dev.positions[2]] = (127 * 256 * 65536) >>> 0;
      expect(ranges, 'min | max << 16 of the param on each knob, 0 on the others').toEqual(want);
      expect(row![ROW_LATCH], 'the latch mask: the word of Mode').toBe(0b10);
      await session.expectNoFault('commit');
    });

    test('more rows than [uses] is E_DECL_USES and writes nothing', async () => {
      await rig.first();
      const file = patched('uses.dkm', (f) => {
        const at = usesCountAt(f, RESOURCE.PARAM_ROWS);
        expect(f.readUInt32BE(at), 'the fixture declares three param rows').toBe(3);
        f.writeUInt32BE(2, at);
      });
      const good = await goodArenaOf(rig);
      const got = await bootEntries(rig, 'commit-uses', [{ name: ID, file, want: E_DECL_USES }]);
      expect(got.info.results).toEqual([E_DECL_USES]);
      await expectNothing(rig, got, [], 0, good);
    });

    test('a machine index that the keys table lacks is E_DECL_ID and writes nothing', async () => {
      await rig.first();
      // init declares machine 0 and effect 0; with a header table that has no entries (and no DSP section, which would name its voice key) there is no row for them
      const file = patched('id.dkm', (f) => {
        f.writeUInt32BE(8, H_KEYS_SIZE);
        f.writeUInt16BE(0, f.readUInt32BE(H_KEYS_OFF) + 6);
        f.writeUInt32BE(0, H_DSP_SIZE);
      });
      const good = await goodArenaOf(rig);
      const got = await bootEntries(rig, 'commit-id', [{ name: ID, file, want: E_DECL_ID }]);
      expect(got.info.results).toEqual([E_DECL_ID]);
      await expectNothing(rig, got, [], 0, good);
    });

    test('an effect id that a loaded module has is E_FX_ID_DUP and the first module stays intact', async () => {
      await rig.first();
      // the copy has another name and another machine key (in the keys table and the DSP voice record), the same effect id
      const copy = patched('dup.dkm', (f) => {
        Buffer.alloc(32).copy(f, H_NAME);
        Buffer.from('test/commit-copy').copy(f, H_NAME);
        expect(replaceAll(f, KEY, KEY2), 'the machine key is in the keys table and the voice record').toBeGreaterThanOrEqual(2);
      });
      const entries: Entry[] = [
        { name: ID, file: moduleArtifact(rig.device, ID), want: LOADED },
        { name: 'test/commit-copy', file: copy, want: E_FX_ID_DUP },
      ];
      const good = await goodArenaOf(rig);
      const got = await bootEntries(rig, 'commit-dup', entries);
      expect(got.info.results[1]).toBe(E_FX_ID_DUP);
      await expectNothing(rig, got, [ID], 1, good);
    });
  },
);
