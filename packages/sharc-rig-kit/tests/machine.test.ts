import { describe, expect, it } from 'vitest';
import { FRAME, OUT, RECORDS, STACK, STOCK_BP, TABLE, UNPACK_BP, f2w, nw } from '../src/context.ts';
import { MachineRig } from '../src/machine.ts';
import type { CallSpec, Op } from '../src/rig.ts';
import { SW_ALIAS_BASE } from '../src/toolchain.ts';
import { built, fakeRig, param, spec } from './fakes.ts';

const pokes = (ops: Op[]) => ops.flatMap((o) => ('poke' in o ? o.poke : []));
const masked = (ops: Op[]) => ops.flatMap((o) => ('pokem' in o ? o.pokem : []));
const calls = (ops: Op[]): CallSpec[] => ops.flatMap((o) => ('call' in o ? [o.call] : []));

describe('MachineRig setup', () => {
  it('loads the linked code, ends a call at the stand-ins and writes the registry row of the machine', async () => {
    const fake = fakeRig('dt2');
    const rig = await MachineRig.open(spec('dt2', [param('a', 1, 0x10, 0x7f00), param('b', 3, 0, 0x10000)]), {
      built: built(),
      open: fake.open,
    });
    const setup = fake.setups[0]!;
    expect(setup.code).toEqual([{ address: SW_ALIAS_BASE + 2 * 0xb90000, hex: '0000' }]);
    expect(setup.breaks).toEqual([STOCK_BP, UNPACK_BP]);
    expect(setup.watch).toBeUndefined();
    const init = pokes(setup.ops ?? []);
    const word = (a: number) => init.findLast(([addr]) => addr === a)?.[1]; // the last write wins
    expect(word(nw(TABLE))).toBe(20); // the machine type
    expect(word(nw(TABLE) + 1)).toBe(0x1000); // the render symbol
    expect(word(nw(TABLE) + 2 + 1)).toBe(0x10 | (0x7f00 << 16)); // the declared range of knob 1
    expect(word(nw(TABLE) + 2 + 3)).toBe(0); // a range that does not fit 16 bits is not declared: the cleared word stays
    // bss runs start as NaN
    expect(word(nw(0x200a0010))).toBe(0xffffffff);
    await rig.close();
    expect(fake.closed()).toBe(true);
  });

  it('asks for the stores of the module code only when it records them', async () => {
    const fake = fakeRig('dt2');
    await (await MachineRig.open(spec('dt2'), { built: built(), open: fake.open, recordStores: true })).close();
    expect(fake.setups[0]!.watch).toEqual([[0x1000, 0x1100]]);
  });
});

describe('MachineRig profile', () => {
  it('asks the rig for per-pc counts only with the option and returns them with the block', async () => {
    const plain = fakeRig('dt2');
    const r0 = await MachineRig.open(spec('dt2'), { built: built(), open: plain.open });
    expect((await r0.block()).profile).toEqual([]);
    expect(calls(plain.batches[0]!)[0]).not.toHaveProperty('profile');
    const fake = fakeRig('dt2', () => ({
      profile: [
        [0x1000, 3],
        [0x1002, 1],
      ],
    }));
    const rig = await MachineRig.open(spec('dt2'), { built: built(), open: fake.open, profile: true });
    const b = await rig.block();
    expect(calls(fake.batches[0]!)[0]!.profile).toBe(true);
    expect(b.profile).toEqual([
      [0x1000, 3],
      [0x1002, 1],
    ]);
  });
});

describe('MachineRig block', () => {
  it('sets a parameter in the frame halfword of its knob, fills the output with NaN and calls the wrapper entry', async () => {
    const fake = fakeRig('dt2');
    const rig = await MachineRig.open(spec('dt2'), { built: built(), open: fake.open });
    rig.setParams([['a', 0x1234]]);
    await rig.block();
    const ops = fake.batches[0]!;
    // knob 1 of track 0 is frame halfword 0x6d + 1 = 110: the low half of word 55
    expect(masked(ops)).toContainEqual([nw(FRAME) + 55, 0xffff, 0x1234]);
    const out = OUT; // voice 0
    expect(pokes(ops)).toContainEqual([nw(out) + 31, 0xffffffff]);
    expect(pokes(ops)).toContainEqual([nw(out + 0x80) + 31, 0xffffffff]); // a DT2 stereo machine's R block
    const call = calls(ops)[0]!;
    expect(call.pc).toBe(0xb90000);
    expect(call.regs.R8).toBe(out);
    expect(call.regs.I6).toBe(STACK);
    expect(call.max_steps).toBe(400_000);
  });

  it('starts a DT2 note by the record word and a DN2 note by the trig bit, which it clears after the block', async () => {
    const dt2 = fakeRig('dt2');
    const a = await MachineRig.open(spec('dt2'), { built: built(), open: dt2.open });
    a.trigger();
    await a.block();
    expect(pokes(dt2.batches[0]!)).toContainEqual([nw(RECORDS + 0x1b8), 1]);

    const dn2 = fakeRig('dn2');
    const b = await MachineRig.open(spec('dn2'), { built: built(), open: dn2.open });
    b.trigger();
    await b.block();
    const bit = (1 << 16) >>> 0; // voice 0: bit 0 of frame halfword 0x11, the upper half of word 8
    expect(masked(dn2.batches[0]!)).toContainEqual([nw(FRAME) + 8, bit, bit]);
    await b.block(); // the clear goes with the next batch, and the trigger is spent
    expect(masked(dn2.batches[1]!)).toContainEqual([nw(FRAME) + 8, bit, 0]);
    expect(masked(dn2.batches[1]!).filter(([a, , v]) => a === nw(FRAME) + 8 && v === bit)).toEqual([]);
  });

  it('reports the samples, the instruction count and the guard count of the call', async () => {
    const fake = fakeRig('dt2', () => ({
      instructions: 1234,
      reads: [Array<number>(32).fill(f2w(0.5)), Array<number>(32).fill(0xffffffff), [0x10003]],
    }));
    const rig = await MachineRig.open(spec('dt2'), { built: built(), open: fake.open });
    const b = await rig.block();
    expect(b.samples).toEqual(Array<number>(32).fill(0.5));
    expect(b.right.every(Number.isNaN)).toBe(true);
    expect(b.instructions).toBe(1234);
    expect(b.faults).toBe(3); // the low 16 bits of the registry word
    expect(b.returned).toBe(true);
    expect(b.contextBad).toEqual([]);
  });

  it('names the context registers that did not come back, only for a call that returned', async () => {
    const wrong = fakeRig('dt2', (_, ops) => {
      const c = calls(ops)[0]!;
      return { regs: Object.fromEntries(c.read_regs.map((n) => [n, n === 'M5' ? c.regs.M5! ^ 1 : c.regs[n]!])) };
    });
    const rig = await MachineRig.open(spec('dt2'), { built: built(), open: wrong.open });
    expect((await rig.block('friendly')).contextBad).toEqual(['M5']);
    const hung = fakeRig('dt2', () => ({ halt: 'max-steps', final_pc: 5 }));
    const stuck = await MachineRig.open(spec('dt2'), { built: built(), open: hung.open });
    const b = await stuck.block();
    expect(b.returned).toBe(false);
    expect(b.contextBad).toEqual([]);
  });

  it('draws a new hostile context for every block and keeps the one it used', async () => {
    const fake = fakeRig('dt2');
    const rig = await MachineRig.open(spec('dt2'), { built: built(), open: fake.open });
    await rig.block();
    const first = { ...rig.lastCtx };
    await rig.block();
    const [c1, c2] = [calls(fake.batches[0]!)[0]!, calls(fake.batches[1]!)[0]!];
    expect(c1.regs.MODE1).toBe(first.MODE1);
    expect(Object.entries(c1.regs).filter(([k]) => k.startsWith('L'))).not.toEqual([]);
    expect(JSON.stringify(c1.regs)).not.toBe(JSON.stringify(c2.regs));
  });
});

describe('MachineRig write set', () => {
  const store = (pc: number, address: number, width = 4): [number, number, number] => [pc, address, width];
  const run = async (stores: [number, number, number][]) => {
    const fake = fakeRig('dt2', () => ({ stores }));
    const rig = await MachineRig.open(spec('dt2'), { built: built(), open: fake.open, recordStores: true });
    return (await rig.block()).foreign;
  };

  it('accepts stores to the output block, the module data and the module stack', async () => {
    expect(await run([store(0x1010, OUT + 8), store(0x1010, 0x200a0014), store(0x1010, STACK - 4 * 40 - 4)])).toEqual(
      [],
    );
  });

  it('ignores a store of code outside the module (the wrapper)', async () => {
    expect(await run([store(0x20, TABLE + 8)])).toEqual([]);
  });

  it('names a foreign store by the region it hit, or by the gap to its own memory', async () => {
    const found = await run([store(0x1010, TABLE + 8), store(0x1010, OUT + 0x100 + 4, 8)]);
    expect(found[0]).toBe(
      `in render: store of 4 bytes to 0x${(TABLE + 8).toString(16)} at pc 0x1010, inside registry +0x8`,
    );
    expect(found[1]).toMatch(/in render: store of 8 bytes to .* at pc 0x1010, inside output buffers \+0x104$/);
  });

  it('reports at most four foreign stores', async () => {
    expect((await run(Array.from({ length: 9 }, (_, i) => store(0x1010, TABLE + 4 * i)))).length).toBe(4);
  });

  it('translates a store at the loader alias back to its address', async () => {
    expect(await run([store(0x1010, SW_ALIAS_BASE + OUT + 4)])).toEqual([]);
  });
});
