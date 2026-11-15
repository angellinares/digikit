import { describe, expect, it } from 'vitest';
import type { CallResult, Op, RigPort } from '../src/rig.ts';
import {
  BACK,
  CALLEE_SAVED,
  CALLER,
  STEREO_FRAME,
  StereoSession,
  bits,
  buildEngine,
  call,
  engineRegs,
  filled,
  openImageSession,
  placeObjectData,
  planes,
} from '../src/stereo.ts';
import { DN2_112, stockPath, stockSkipReason } from '../src/stock.ts';
import type { RigSetup } from '../src/rig.ts';

type Call = Extract<Op, { call: unknown }>['call'];

/** A port that remembers the ops and answers every read with the address of the word (so a test sees which words were asked). */
function port(answer: (c: Call) => Partial<CallResult> = () => ({})): { port: RigPort; ops: Op[]; calls: Call[] } {
  const ops: Op[] = [];
  const calls: Call[] = [];
  return {
    ops,
    calls,
    port: {
      async run(batch) {
        ops.push(...batch);
        const out: CallResult[] = [];
        for (const op of batch) {
          if (!('call' in op)) continue;
          calls.push(op.call);
          out.push({
            instructions: 7,
            halt: 'breakpoint',
            final_pc: BACK,
            regs: Object.fromEntries(op.call.read_regs.map((n) => [n, op.call.regs[n] ?? 0])),
            reads: op.call.reads.map(([a, n]) => Array.from({ length: n }, (_, k) => a + k)),
            stores: [],
            ...answer(op.call),
          });
        }
        return out;
      },
      async close() {},
    },
  };
}

describe('StereoSession', () => {
  it('reads a range word by word (an address this large is a byte address) and keeps a stopped call resumable', async () => {
    const { port: p, calls } = port();
    const s = new StereoSession(p);
    expect(
      await s.peekMany([
        [0x20000000, 3],
        [0x80100000, 2],
      ]),
    ).toEqual([
      [0x20000000, 0x20000004, 0x20000008],
      [0x80100000, 0x80100004],
    ]);
    expect(calls).toHaveLength(1);
    expect(calls[0]!.reads).toEqual([
      [0x20000000, 1],
      [0x20000004, 1],
      [0x20000008, 1],
      [0x80100000, 1],
      [0x80100004, 1],
    ]);
    expect(calls[0]!.resume, 'a peek must not disturb the stacks of a call that stopped at a break').toBe(true);
    expect(calls[0]!.max_steps).toBe(0);
  });

  it('sends the queued pokes before a call and the call options with it', async () => {
    const { port: p, ops } = port();
    const s = new StereoSession(p);
    s.poke(0x2010f000, 0x3f800000);
    s.pokeWords(0x20100000, [1, -1]);
    await s.raw(0x1c9aea, { R9: 32 }, { breaks: [BACK, 0x1c9b30], maxSteps: 4000, readRegs: ['I2'], resume: true });
    expect(ops[0]).toEqual({
      poke: [
        [0x2010f000, 0x3f800000],
        [0x20100000, 1],
        [0x20100004, 0xffffffff],
      ],
    });
    expect((ops[1] as { call: Call }).call).toMatchObject({
      pc: 0x1c9aea,
      regs: { R9: 32 },
      max_steps: 4000,
      read_regs: ['I2'],
      breaks: [BACK, 0x1c9b30],
      resume: true,
    });
  });

  it('passes multiplier registers to the call and keeps what the core answers', async () => {
    const msf: [string, string] = ['f'.repeat(20), '1234'];
    const { port: p, calls } = port(() => ({ specials: { MSF: msf } }));
    const s = new StereoSession(p);
    const r = await s.raw(1, {}, { specials: { MSF: msf }, readSpecials: ['MSF'] });
    expect(calls[0]!.specials).toEqual({ MSF: msf });
    expect(calls[0]!.read_specials).toEqual(['MSF']);
    expect(r.specials).toEqual({ MSF: msf });
  });

  it('answers stock reads from the image as loaded', async () => {
    const { port: p, calls } = port(() => ({ stock: ['04', null] }));
    const s = new StereoSession(p);
    expect(
      await s.stock([
        [0x28000000, 1],
        [0x28000010, 4],
      ]),
    ).toEqual(['04', null]);
    expect(calls[0]!.stock_reads).toEqual([
      [0x28000000, 1],
      [0x28000010, 4],
    ]);
  });

  it('closes its port once', async () => {
    let closed = 0;
    const s = new StereoSession({ run: async () => [], close: async () => void closed++ });
    await s.close();
    await s.close();
    expect(closed).toBe(1);
  });
});

describe('call', () => {
  it('builds the engine frame, sets the sentinels and checks the callee-saved registers', async () => {
    const { port: p, ops, calls } = port();
    const s = new StereoSession(p);
    const r = await call(s, 0xb80c39, 0x100, 0x200, 0x300, [32, 7], { regs: { I0: 5 } });
    expect(r.instructions).toBe(7);
    expect(ops[0]).toEqual({
      poke: [
        [STEREO_FRAME, CALLER],
        [STEREO_FRAME - 4, BACK - 1],
        [STEREO_FRAME + 4, 32],
        [STEREO_FRAME + 8, 7],
      ],
    });
    const regs = calls[0]!.regs;
    expect(regs).toMatchObject({
      R4: 0x100,
      R8: 0x200,
      R12: 0x300,
      I6: STEREO_FRAME,
      I7: STEREO_FRAME - 8,
      R2: CALLER,
      I0: 5,
      R3: 0x3000,
      M6: 1,
    });
    expect(regs.I1).toBe(0x20000100 + 16 * CALLEE_SAVED.indexOf('I1'));
    expect(calls[0]!.read_regs).toEqual([...CALLEE_SAVED]);
  });

  it('fails when a callee-saved register changes, and when the call does not return', async () => {
    const changed = port((c) => ({ regs: { ...Object.fromEntries(c.read_regs.map((n) => [n, c.regs[n]!])), R5: 1 } }));
    await expect(call(new StereoSession(changed.port), 1, 0, 0, 0, [])).rejects.toThrow('R5 is callee-saved');
    const stuck = port(() => ({ halt: 'max-steps', final_pc: 0x1234 }));
    await expect(call(new StereoSession(stuck.port), 1, 0, 0, 0, [])).rejects.toThrow('did not return');
    // without sentinels the registers are not compared
    const free = port(() => ({ regs: { R5: 1 } }));
    await expect(call(new StereoSession(free.port), 1, 0, 0, 0, [], { sentinels: false })).resolves.toBeDefined();
  });
});

describe('openImageSession and placeObjectData', () => {
  it('places each section at its alignment and pokes every word of a run, a partial last word included, the BSS as NaN words when poisoned', async () => {
    const object = {
      text: '',
      parcels: 0,
      symbols: {},
      data: {
        '.A': { bytes: Uint8Array.from([1, 2, 3, 4, 5, 6]), bss: false, align: 4 },
        '.B': { bytes: new Uint8Array(8), bss: true, align: 16 },
      },
      dataSymbols: { _a: { section: '.A', offset: 2 }, _b: { section: '.B', offset: 4 } },
    };
    const { data, inits, end } = placeObjectData(object, 0x200d0000);
    expect(data).toEqual({ _a: 0x200d0002, _b: 0x200d0014 });
    expect(end).toBe(0x200d0010 + 8);
    const setups: RigSetup[] = [];
    const open = async (setup: RigSetup) => {
      setups.push(setup);
      return { port: port().port, first: [] };
    };
    await openImageSession({ code: [], inits, pokes: [[0x100, 7]], watch: [[0, 0x1000000]], open });
    const pokes = (setups[0]!.ops![0] as { poke: [number, number][] }).poke;
    expect(pokes).toEqual([
      [0x200d0000, 0x04030201],
      [0x200d0004, 0x0605],
      [0x200d0010, 0xffffffff],
      [0x200d0014, 0xffffffff],
      [0x100, 7],
    ]);
    expect(setups[0]!.watch).toEqual([[0, 0x1000000]]);
    expect(setups[0]!.image?.length).toBeGreaterThan(0);
  });
});

describe('the engine data', () => {
  it('pokes 16 slots, the fade flags as bytes and the context registers of a block run', async () => {
    const { port: p, ops } = port();
    const s = new StereoSession(p);
    const left = Object.fromEntries(Array.from({ length: 16 }, (_, v) => [v, filled(32, bits(v))]));
    buildEngine(s, left, { fade: new Set([1, 5]) });
    await s.peek(0);
    const pokes = (ops[0] as { poke: [number, number][] }).poke;
    // slot 1's flag is byte 1 of word 0 of the array, slot 5 byte 1 of word 1
    const flags = pokes.filter(([a]) => a >= 0x20110000 + 0x138fc && a < 0x20110000 + 0x138fc + 16);
    expect(flags.slice(-4).map(([, w]) => w)).toEqual([0x100, 0x100, 0, 0]);
    const regs = engineRegs({ R9: 1 });
    expect(regs).toMatchObject({ R9: 1, I9: 0x20110000, M6: 1, M7: 0xffffffff, B15: 0 });
  });

  it('reads the planes of 16 slots as two blocks each', async () => {
    const { port: p } = port();
    const rows = await planes(new StereoSession(p));
    expect(rows).toHaveLength(16);
    expect(rows[1]![0][0]).toBe(0x20180000 + 0x100);
    expect(rows[1]![1][0]).toBe(0x20180000 + 0x100 + 0x80);
    expect(rows[0]![0]).toHaveLength(32);
  });
});

describe('the stock image', () => {
  it('names the extracted section and the reason a test is skipped without it', () => {
    expect(stockPath(DN2_112)).toMatch(/out\/sections\/dn2-1\.12\/section_7_BLOB\.bin$/);
    expect(stockSkipReason(DN2_112)).toContain('requires the original dn2-1.12 section 7');
  });
});
