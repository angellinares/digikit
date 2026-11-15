import { describe, expect, it } from 'vitest';
import { STACK, STOCK_BP, TABLE, f2w, nw } from '../src/context.ts';
import { AMT0, DESC, ENABLED, FxRig, MIX, P0, PC, ROW_WORDS, STATE } from '../src/fx.ts';
import { FX_MASTER } from '../src/place.ts';
import type { CallSpec, Op } from '../src/rig.ts';
import { SW_ALIAS_BASE } from '../src/toolchain.ts';
import { fakeRig, fxBuilt } from './fakes.ts';

const pokes = (ops: Op[]) => ops.flatMap((o) => ('poke' in o ? o.poke : []));
const calls = (ops: Op[]): CallSpec[] => ops.flatMap((o) => ('call' in o ? [o.call] : []));
const ROW = nw(TABLE);
// the hook ends in the stock function's second instruction
const stock = { final_pc: STOCK_BP };

describe('FxRig setup', () => {
  it('writes the chain table: a row per effect, a free row for an empty slot, the state symbol or the rig-reserved state', async () => {
    const fake = fakeRig('dt2');
    const rig = await FxRig.open(
      [
        { pc: '_te_half', state: '_te_state_a', params: [5, 6], mix: 0x3f000000 },
        { pc: '_te_half', state: '' },
        { pc: '', state: '' },
      ],
      { sources: [], built: fxBuilt(), open: fake.open },
    );
    const setup = fake.setups[0]!;
    expect(setup.breaks).toEqual([STOCK_BP]);
    expect(setup.code[0]!.address).toBe(SW_ALIAS_BASE + 2 * 0xb90000);
    const init = pokes(setup.ops ?? []);
    const word = (a: number) => init.findLast(([addr]) => addr === a)?.[1];
    expect(word(ROW + PC)).toBe(0x1000);
    expect(word(ROW + DESC)).toBe(0x200a0000);
    expect(word(ROW + STATE)).toBe(0x200a0100);
    expect(word(ROW + ENABLED)).toBe(1);
    expect(word(ROW + MIX)).toBe(0x3f000000);
    expect([word(ROW + P0), word(ROW + P0 + 1), word(ROW + P0 + 2)]).toEqual([5, 6, 0]);
    expect(word(ROW + AMT0)).toBe(0);
    expect(word(ROW + ROW_WORDS + MIX)).toBe(0x3f800000); // the default mix is 1.0
    expect(word(ROW + ROW_WORDS + STATE)).toBe(0x200a0200); // `_fx_auto_state1`
    expect(word(ROW + 2 * ROW_WORDS + PC)).toBe(0xffffffff); // a free row
    // bss runs start poisoned with a NaN that is not the NaN of the machine rig
    expect(word(nw(0x200a0100))).toBe(0x7fc00001);
    await rig.close();
  });

  it('refuses a chain of more than four effects', async () => {
    const row = { pc: '_te_half', state: '_te_state_a' };
    await expect(
      FxRig.open(Array(5).fill(row), { sources: [], built: fxBuilt(), open: fakeRig('dt2').open }),
    ).rejects.toThrow('at most 4');
  });

  it('reads the words of a description from its initial bytes', async () => {
    const rig = await FxRig.open([], { sources: [], built: fxBuilt(), open: fakeRig('dt2').open });
    expect(rig.descWords('_te_desc', 3)).toEqual([0x444b4658, 1, 3]);
    expect(() => rig.descWords('_nope')).toThrow('no data symbol');
  });
});

describe('FxRig block', () => {
  const open = async (reply = {}) => {
    const fake = fakeRig('dt2', () => ({ ...stock, ...reply }));
    const rig = await FxRig.open([{ pc: '_te_half', state: '_te_state_a' }], {
      sources: [],
      built: fxBuilt(),
      open: fake.open,
    });
    return { fake, rig };
  };

  it('writes the master block as floats, a parameter word and the enable flag before the call', async () => {
    const { fake, rig } = await open();
    rig.setRowWord(0, P0 + 1, 0x1234);
    rig.setEnabled(0, false);
    await rig.block(Array.from({ length: 64 }, (_, k) => k / 64));
    const ops = fake.batches[0]!;
    expect(pokes(ops)).toContainEqual([nw(FX_MASTER) + 2, f2w(2 / 64)]);
    expect(pokes(ops)).toContainEqual([ROW + P0 + 1, 0x1234]);
    expect(pokes(ops)).toContainEqual([ROW + ENABLED, 0]);
    const call = calls(ops)[0]!;
    expect(call.pc).toBe(0xb90000);
    expect(call.regs.I6).toBe(STACK);
    expect(call.regs.R3).toBe(0x7003); // a marker the stock function expects back
    expect(call.read_regs).toEqual(expect.arrayContaining(['I6', 'I7', 'R15', 'B3', 'MODE1']));
  });

  it('reports the master after the hook, the fault count of each row and the effect output block', async () => {
    const master = Array.from({ length: 64 }, (_, k) => f2w(k));
    const { rig } = await open({
      instructions: 777,
      reads: [master, [0], [2], [0], [0], Array<number>(64).fill(f2w(0.25))],
    });
    const b = await rig.block(Array(64).fill(0));
    expect(b.master[5]).toBe(5);
    expect(b.instructions).toBe(777);
    expect(b.faults).toEqual([0, 2, 0, 0]);
    expect(b.scratch).toEqual(Array<number>(64).fill(0.25));
    expect(b.returned).toBe(true);
  });

  it('names the marked registers and the context registers that did not come back, only for a call that returned', async () => {
    const wrong = fakeRig('dt2', (_, ops) => {
      const c = calls(ops)[0]!;
      const back = Object.fromEntries(c.read_regs.map((n) => [n, c.regs[n] ?? STACK]));
      return { ...stock, regs: { ...back, R3: 0, M5: back.M5! ^ 1, I7: 0 } };
    });
    const rig = await FxRig.open([{ pc: '_te_half', state: '_te_state_a' }], {
      sources: [],
      built: fxBuilt(),
      open: wrong.open,
    });
    const b = await rig.block(Array(64).fill(0), 'friendly');
    expect(b.ctxBad).toEqual(['M5']);
    expect(b.regsBad).toEqual(['R3', 'I7']);
    const hung = fakeRig('dt2', () => ({ halt: 'max-steps', final_pc: 3 }));
    const stuck = await FxRig.open([], { sources: [], built: fxBuilt(), open: hung.open });
    const s = await stuck.block(Array(64).fill(0));
    expect([s.returned, s.ctxBad, s.regsBad]).toEqual([false, [], []]);
  });

  it('draws a hostile context for each block', async () => {
    const { fake, rig } = await open();
    await rig.block(Array(64).fill(0));
    await rig.block(Array(64).fill(0));
    const [a, b] = [calls(fake.batches[0]!)[0]!, calls(fake.batches[1]!)[0]!];
    expect(JSON.stringify(a.regs)).not.toBe(JSON.stringify(b.regs));
  });
});

describe('FxRig write set', () => {
  const run = async (stores: [number, number, number][]) => {
    const fake = fakeRig('dt2', () => ({ ...stock, stores }));
    const rig = await FxRig.open([], { sources: [], built: fxBuilt(), open: fake.open, recordStores: true });
    const b = await rig.block(Array(64).fill(0));
    return rig.foreignStores(16, b.stores);
  };

  it('accepts stores to the output block, the module data and the stack the module declared', async () => {
    expect(
      await run([
        [0x1010, 0x200a0300 + 4, 4],
        [0x1010, 0x200a0104, 4],
        [0x1010, STACK - 8, 4],
      ]),
    ).toEqual([]);
  });

  it('names a store into the master block or the chain table', async () => {
    const found = await run([
      [0x1010, FX_MASTER + 4, 4],
      [0x1010, TABLE + 0x10, 4],
      [0x1010, 0x20000000, 4],
    ]);
    expect(found[0]).toBe(
      `store of 4 bytes to 0x${(FX_MASTER + 4).toString(16)} at pc 0x1010, inside the master block (read only for an effect) +0x4`,
    );
    expect(found[1]).toMatch(/inside the chain table \+0x10$/);
    expect(found[2]).toMatch(/unknown memory$/);
  });

  it('ignores a store of the adapter code', async () => {
    expect(await run([[0x20, FX_MASTER, 4]])).toEqual([]);
  });
});

describe('FxRig send route', () => {
  const sendBuilt = () => {
    const b = fxBuilt();
    return { ...b, symbols: { ...b.symbols, _fxs_entry: 0xb90040 }, data: { ...b.data, _fxs_bus: 0x200a0600 } };
  };

  it('packs the 16 amount bytes of a row, track 4w + b in byte b of word w, and clamps nothing but the byte', async () => {
    const fake = fakeRig('dt2');
    const amounts = Array.from({ length: 16 }, (_, t) => (t === 5 ? 0x1ff : t + 1));
    await FxRig.open([{ pc: '_te_half', state: '_te_state_a', route: 1, amounts }], {
      sources: [],
      built: sendBuilt(),
      open: fake.open,
    });
    const init = pokes(fake.setups[0]!.ops ?? []);
    const word = (w: number) => init.findLast(([addr]) => addr === ROW + AMT0 + w)?.[1];
    expect([word(0), word(1), word(2), word(3)]).toEqual([0x04030201, 0x0807ff05, 0x0c0b0a09, 0x100f0e0d]);
  });

  it('runs the send hook entry with the buses set before the call, and reads the return bus after it', async () => {
    const fake = fakeRig('dt2', () => stock);
    const rig = await FxRig.open([{ pc: '_te_half', state: '_te_state_a' }], {
      sources: [],
      built: sendBuilt(),
      open: fake.open,
    });
    rig.setSlotmap([3, 4, 5]);
    rig.setFade([1, 4]);
    rig.setReturn([1.0]);
    await rig.sendBlock();
    const ops = fake.batches[0]!;
    expect(calls(ops)[0]!.pc).toBe(0xb90040);
    expect(pokes(ops)).toContainEqual([nw(0x2009e800), 3 | (4 << 16)]);
    expect(pokes(ops)).toContainEqual([nw(0x2009e800) + 1, 5]);
    expect(pokes(ops)).toContainEqual([nw(0x2009e820), 0x00000100]); // slot 1 flagged: byte 1 of the first fade word
    expect(pokes(ops)).toContainEqual([nw(0x2009e820) + 1, 0x00000001]);
    expect(pokes(ops)).toContainEqual([nw(0x2009e400), f2w(1.0)]);
    expect(calls(ops)[0]!.reads.at(-1)).toEqual([nw(0x2009e400), 64]);
  });

  it('peeks a word as a call at the return point that runs no instruction, after the pending pokes', async () => {
    const fake = fakeRig('dt2');
    const rig = await FxRig.open([{ pc: '_te_half', state: '_te_state_a' }], {
      sources: [],
      built: sendBuilt(),
      open: fake.open,
    });
    rig.setRowWord(0, MIX, 7);
    await rig.rowWord(0, MIX);
    const ops = fake.batches[0]!;
    expect(ops.map((o) => Object.keys(o)[0])).toEqual(['poke', 'call']);
    expect(calls(ops)[0]).toMatchObject({ pc: 0xb9f100, max_steps: 0, reads: [[ROW + MIX, 1]] });
  });
});
