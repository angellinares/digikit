/**
 * core/fx, the send route (modules/core/adapters/fx/src/dsp/fxsend.s, finding 62): the send hook and the master hook on the SHARC-only rig.
 * Port of the rig tests of test_fx_send.py (T1g phase 5); the two tests that patch the stock Digitakt II image stay in Python.
 *
 * The model of the send bus is the routine's own float32 arithmetic: gain (byte / 127) squared, ramped by adding the step per sample,
 * tracks summed in order.
 */
import { describe, expect, it } from 'vitest';
import { DIS, FxRig, NFAULT, PyRandom, ROUTE_SEND, TRACKS, WAS, FX_RETURN, TABLE, f2w, type Row } from '@digi/sharc-rig-kit';
import {
  AMOUNTS,
  ONE,
  POISON,
  RAMP,
  SIGNALS,
  addReturn,
  amountsOf,
  clean,
  f32,
  free,
  gain,
  half,
  load,
  open,
  same,
  sendBus,
  sendRow,
  withRig,
  ramp01,
  zeros,
} from './fx-send-model.ts';

const SW_ALIAS_BASE = 0x28000000;

/** One real block: the send hook, then the master hook. */
async function realBlock(rig: FxRig): Promise<void> {
  await rig.sendBlock();
  await rig.block(RAMP);
}


describe('core/fx send route: the master chain skips send rows', () => {
  it(
    'a send row is not run by the master chain',
    withRig([sendRow()], async (rig) => {
      for (let i = 0; i < 3; i++) {
        const b = await rig.block(RAMP);
        clean(b);
        same(b.master, RAMP);
      }
      expect(await rig.stateWord(0, 0)).toBe(POISON); // never touched
      expect(await rig.rowWord(0, WAS)).toBe(0);
    }),
  );

  it('a send row costs the master hook the quick scan and the count only', async () => {
    const base = await (async () => {
      const r = await open([free]);
      try {
        return (await r.block(RAMP, 'friendly')).instructions;
      } finally {
        await r.close();
      }
    })();
    const rig = await open([sendRow()]);
    try {
      const b = await rig.block(RAMP, 'friendly');
      console.log(`master hook, one send row: ${b.instructions} instructions (a free row: ${base})`);
      expect(b.instructions).toBeLessThanOrEqual(base + 10);
    } finally {
      await rig.close();
    }
  });

  it(
    'the master chain runs the master row beside a send row',
    withRig([sendRow(), { pc: '_te_add1', state: '_te_state_b' }], async (rig) => {
      await rig.block(RAMP);
      const b = await rig.block(RAMP);
      same(b.master, RAMP.map((x) => f32(x + 1)));
      expect(await rig.stateWord(0, 0)).toBe(POISON);
      expect(await rig.stateWord(1, 0)).toBe(2);
    }),
  );
});

describe('core/fx send route: the block counter', () => {
  it('the block number advances once a block with or without a send row', async () => {
    const n = 6;
    const param = { pc: '_te_param', params: [7, 0, 0, 0] };
    const alone = await open([{ ...param, state: '_te_state_a' }]);
    const seenAlone: number[] = [];
    try {
      for (let i = 0; i < n; i++) {
        await alone.block(RAMP);
        seenAlone.push(await alone.stateWord(0, 1));
      }
    } finally {
      await alone.close();
    }
    const both = await open([
      { ...param, state: '_te_state_a' },
      { ...param, state: '_te_state_b', route: ROUTE_SEND },
    ]);
    const seenMaster: number[] = [];
    const seenSend: number[] = [];
    try {
      for (let i = 0; i < n; i++) {
        await both.sendBlock();
        seenSend.push(await both.stateWord(1, 1));
        await both.block(RAMP);
        seenMaster.push(await both.stateWord(0, 1));
      }
    } finally {
      await both.close();
    }
    expect(seenAlone).toEqual(Array.from({ length: n }, (_, i) => i + 1));
    expect(seenMaster).toEqual(seenAlone);
    expect(seenSend).toEqual(Array.from({ length: n }, (_, i) => i));
  });

  it(
    'a chain of send rows alone still counts the blocks',
    withRig([{ pc: '_te_param', state: '_te_state_a', route: ROUTE_SEND, params: [7, 0, 0, 0] }], async (rig) => {
      const seen: number[] = [];
      for (let i = 0; i < 5; i++) {
        await rig.sendBlock();
        seen.push(await rig.stateWord(0, 1));
        const b = await rig.block(RAMP);
        clean(b);
        same(b.master, RAMP);
      }
      expect(seen).toEqual([0, 1, 2, 3, 4]);
      expect(await rig.symbolWord('_fx_blk')).toBe(5);
    }),
  );

  it(
    'a free row and a send row before a master row count once',
    withRig(
      [
        free,
        { pc: '_te_param', state: '_te_state_a', route: ROUTE_SEND, params: [7, 0, 0, 0] },
        { pc: '_te_param', state: '_te_state_b', params: [7, 0, 0, 0] },
      ],
      async (rig) => {
        for (let n = 1; n < 5; n++) {
          await realBlock(rig);
          expect(await rig.stateWord(2, 1), `block ${n}`).toBe(n);
          expect(await rig.symbolWord('_fx_blk'), `block ${n}`).toBe(n);
        }
      },
    ),
  );

  const faultRig = async (): Promise<FxRig> => {
    const rig = await open([
      { pc: '_te_nan', state: '_te_state_a', route: ROUTE_SEND, amounts: AMOUNTS },
      { pc: '_te_half', state: '_te_state_b' },
    ]);
    load(rig);
    return rig;
  };

  it('the fault window is 1500 real blocks with a send row', async () => {
    // seven faults, 1000 real blocks off, the eighth fault: inside the window, so the effect is switched off (with the counter
    // advancing twice a block the window would have been 750 blocks and the count would have started again)
    const rig = await faultRig();
    try {
      for (let i = 0; i < 7; i++) await realBlock(rig);
      expect(await rig.rowWord(0, NFAULT)).toBe(7);
      expect(await rig.rowWord(0, DIS)).toBe(0);
      rig.setEnabled(0, false);
      for (let i = 0; i < 1000; i++) await realBlock(rig);
      rig.setEnabled(0, true);
      await realBlock(rig);
      expect(await rig.rowWord(0, NFAULT)).toBe(8);
      expect(await rig.rowWord(0, DIS)).toBe(1);
    } finally {
      await rig.close();
    }
  });

  it('faults more than 1500 real blocks apart do not switch a send effect off', async () => {
    const rig = await faultRig();
    try {
      for (let i = 0; i < 7; i++) await realBlock(rig);
      rig.setEnabled(0, false);
      for (let i = 0; i < 1600; i++) await realBlock(rig);
      rig.setEnabled(0, true);
      await realBlock(rig);
      expect(await rig.rowWord(0, NFAULT)).toBe(8);
      expect(await rig.rowWord(0, DIS)).toBe(0);
    } finally {
      await rig.close();
    }
  });
});

describe('core/fx send route: the send hook', () => {
  it('no send row leaves the return and the master alone', async () => {
    const rng = new PyRandom(5);
    const rig = await open([{ pc: '_te_half', state: '_te_state_a' }, free]);
    try {
      load(rig);
      for (let n = 0; n < 8; n++) {
        const ret = Array.from({ length: 64 }, () => f32(rng.uniform(-1, 1)));
        const master = Array.from({ length: 64 }, () => f32(rng.uniform(-1, 1)));
        rig.setReturn(ret);
        const b = await rig.sendBlock(master);
        clean(b, `block ${n}`);
        same(b.ret, ret);
        same(b.master, master);
      }
      expect(await rig.stateWord(0, 0)).toBe(POISON);
    } finally {
      await rig.close();
    }
  });

  it('an empty chain and free rows cost the send hook a few instructions', async () => {
    const chains: Row[][] = [[], [free, free, free], [{ pc: '_te_half', state: '_te_state_a' }]];
    for (const rows of chains) {
      const rig = await open(rows);
      try {
        const b = await rig.sendBlock(undefined, 'friendly');
        console.log(`send hook, ${rows.length} rows, none routed to send: ${b.instructions}`);
        expect(b.returned).toBe(true);
        expect(b.instructions).toBeLessThanOrEqual(46);
      } finally {
        await rig.close();
      }
    }
  });

  it(
    'the send bus is the sum of the gained tracks and the return gets the wet bus',
    withRig([sendRow()], async (rig) => {
      load(rig);
      let old = zeros(TRACKS);
      const ret = ramp01;
      rig.setReturn(ret);
      // block 0: the row switches on (fade in), the gains ramp from 0
      let bus: number[];
      [bus, old] = sendBus(AMOUNTS, old);
      let b = await rig.sendBlock();
      clean(b);
      same(await rig.bus(), bus);
      same(b.ret, addReturn(ret, half(bus), 1.0, 'in'));
      same(await rig.prevGains(0), old);
      // the tracks and the master are not touched
      same(b.master, zeros(64));
      // block 1 and 2: steady, constant gains
      for (let i = 0; i < 2; i++) {
        rig.setReturn(ret);
        [bus, old] = sendBus(AMOUNTS, old);
        b = await rig.sendBlock();
        same(await rig.bus(), bus);
        same(b.ret, addReturn(ret, half(bus), 1.0, 'steady'));
      }
      expect(await rig.rowWord(0, WAS)).toBe(1);
    }),
  );

  it(
    'only tracks with an amount contribute',
    withRig([sendRow({ amounts: amountsOf(5, 64) })], async (rig) => {
      load(rig);
      await rig.sendBlock(); // the ramp block
      await rig.sendBlock();
      const g = gain(64);
      same(await rig.bus(), SIGNALS[5]!.map((x) => f32(x * g)));
      rig.setAmounts(0, amountsOf(6, 64));
      await rig.sendBlock(); // track 5 ramps down, track 6 up
      await rig.sendBlock();
      same(await rig.bus(), SIGNALS[6]!.map((x) => f32(x * g)));
    }),
  );

  it('the gain is the amount squared over 127 squared', async () => {
    for (const byte of [0, 1, 32, 64, 100, 127, 128, 255]) {
      const rig = await open([sendRow({ amounts: amountsOf(0, byte) })]);
      try {
        load(rig);
        await rig.sendBlock();
        await rig.sendBlock();
        const want = Math.min(byte, 127) ** 2 / 127 ** 2;
        const got = (await rig.prevGains(0))[0]!;
        expect(Math.abs(got - want), `byte ${byte}: ${got} against ${want}`).toBeLessThan(2e-7);
      } finally {
        await rig.close();
      }
    }
  });

  it(
    'a change of amount ramps over the block',
    withRig([sendRow({ amounts: amountsOf(2, 127) })], async (rig) => {
      load(rig);
      await rig.sendBlock();
      await rig.sendBlock();
      const old = amountsOf(2, 127).map(gain);
      const newAmounts = amountsOf(2, 32);
      rig.setAmounts(0, newAmounts);
      const [bus] = sendBus(newAmounts, old);
      await rig.sendBlock();
      same(await rig.bus(), bus);
      // linear: the first sample is near the old gain, the last is the new gain
      const x = SIGNALS[2]!;
      expect(Math.abs(bus[0]! / x[0]! - 1)).toBeLessThan(0.05);
      expect(Math.abs(bus[31]! / x[31]! - gain(32))).toBeLessThan(1e-6);
      await rig.sendBlock();
      same(await rig.bus(), SIGNALS[2]!.map((v) => f32(v * gain(32))));
    }),
  );

  it(
    'all amounts zero leave the return bit for bit',
    withRig([sendRow({ amounts: zeros(TRACKS) })], async (rig) => {
      const rng = new PyRandom(8);
      load(rig);
      for (let i = 0; i < 5; i++) {
        const ret = Array.from({ length: 64 }, () => f32(rng.uniform(-1, 1)));
        rig.setReturn(ret);
        const b = await rig.sendBlock();
        same(b.ret, ret);
        same(await rig.bus(), zeros(64));
      }
      expect(await rig.stateWord(0, 0)).toBe(5); // the effect runs (its tail would go on)
    }),
  );

  it('mix is the return level', async () => {
    for (const mix of [0, 0.25, 1]) {
      const rig = await open([sendRow({ mix: f2w(mix) })]);
      try {
        load(rig);
        const ret = ramp01;
        let old = zeros(TRACKS);
        for (let n = 0; n < 3; n++) {
          rig.setReturn(ret);
          let bus: number[];
          [bus, old] = sendBus(AMOUNTS, old);
          const b = await rig.sendBlock();
          const mode = n === 0 ? 'in' : 'steady';
          same(b.ret, mix === 0 ? ret : addReturn(ret, half(bus), mix, mode), `mix ${mix} block ${n}`);
        }
        expect(await rig.stateWord(0, 0)).toBe(3); // mix 0 still runs the effect
      } finally {
        await rig.close();
      }
    }
  });

  it(
    'the effect fades out over one block and is not called while off',
    withRig([sendRow()], async (rig) => {
      load(rig);
      let old = zeros(TRACKS);
      const ret = ramp01;
      let bus: number[] = [];
      for (let i = 0; i < 2; i++) {
        rig.setReturn(ret);
        [bus, old] = sendBus(AMOUNTS, old);
        await rig.sendBlock();
      }
      rig.setEnabled(0, false);
      rig.setReturn(ret);
      [bus, old] = sendBus(AMOUNTS, old);
      let b = await rig.sendBlock();
      same(b.ret, addReturn(ret, half(bus), 1.0, 'out'));
      expect(b.ret[31]).toBe(ret[31]);
      expect(b.ret[63]).toBe(ret[63]); // the last sample adds 0
      const calls = await rig.stateWord(0, 0);
      for (let i = 0; i < 4; i++) {
        rig.setReturn(ret);
        b = await rig.sendBlock();
        same(b.ret, ret);
      }
      expect(await rig.stateWord(0, 0)).toBe(calls);
      expect(await rig.rowWord(0, WAS)).toBe(0);
    }),
  );

  it(
    'a row that starts off is skipped by the send hook',
    withRig([sendRow({ enabled: 0 })], async (rig) => {
      load(rig);
      rig.setReturn(ramp01);
      const b = await rig.sendBlock();
      same(b.ret, ramp01);
      expect(await rig.stateWord(0, 0)).toBe(POISON);
    }),
  );

  it('a free row in front and a second send row', async () => {
    const rows = [free, sendRow(), sendRow({ amounts: amountsOf(0, 127) })];
    const rig = await open(rows);
    try {
      load(rig);
      const olds = [zeros(TRACKS), zeros(TRACKS)];
      for (let n = 0; n < 3; n++) {
        rig.setReturn(ramp01);
        let want = [...ramp01];
        for (const i of [1, 2]) {
          let bus: number[];
          [bus, olds[i - 1]!] = sendBus(rows[i]!.amounts!, olds[i - 1]!);
          want = addReturn(want, half(bus), 1.0, n === 0 ? 'in' : 'steady');
        }
        const b = await rig.sendBlock();
        clean(b, `block ${n}`);
        same(b.ret, want, `block ${n}`);
      }
      expect((await rig.prevGains(1))[3]).toBe(gain(127));
      expect((await rig.prevGains(2))[0]).toBe(gain(127));
    } finally {
      await rig.close();
    }
  });

  for (const effect of ['_te_nan', '_te_big', '_te_neg'])
    it(
      `a bad block adds nothing to the return: ${effect}`,
      withRig([{ pc: effect, state: '_te_state_a', route: ROUTE_SEND, amounts: AMOUNTS }], async (rig) => {
        load(rig);
        for (let i = 0; i < 3; i++) {
          rig.setReturn(ramp01);
          const b = await rig.sendBlock();
          clean(b);
          same(b.ret, ramp01);
        }
        expect(await rig.rowWord(0, NFAULT)).toBe(3);
        expect(await rig.rowWord(0, WAS)).toBe(0);
      }),
    );

  it(
    'the eighth fault switches a send effect off',
    withRig([{ pc: '_te_nan', state: '_te_state_a', route: ROUTE_SEND, amounts: AMOUNTS }], async (rig) => {
      load(rig);
      for (let i = 0; i < 8; i++) await rig.sendBlock();
      expect(await rig.rowWord(0, NFAULT)).toBe(8);
      for (let i = 0; i < 3; i++) await rig.sendBlock();
      expect(await rig.rowWord(0, NFAULT)).toBe(8); // not called any more
    }),
  );

  it(
    'the send hook gives every register and the context back in hostile contexts',
    withRig([sendRow(), { pc: '_te_trash', state: '_te_state_b', route: ROUTE_SEND }], async (rig) => {
      const rng = new PyRandom(3);
      load(rig);
      for (let n = 0; n < 24; n++) {
        const master = Array.from({ length: 64 }, () => f32(rng.uniform(-1.5, 1.5)));
        rig.setReturn(Array.from({ length: 64 }, () => f32(rng.uniform(-1, 1))));
        rig.setAmounts(0, Array.from({ length: TRACKS }, () => rng.randRange(0, 256)));
        const b = await rig.sendBlock(master);
        clean(b, `block ${n}`);
        same(b.master, master);
      }
    }),
  );

  it(
    'the send hook stores only in its own memory',
    withRig(
      [sendRow()],
      async (rig) => {
        // the stores of a send block: the adapter's scratch, the send bus, gains and scalars, the effect's output and state, the return bus
        load(rig);
        await rig.sendBlock(undefined, 'friendly');
        const b = await rig.sendBlock(undefined, 'friendly');
        const d = rig.built.data;
        const allowed: [number, number][] = (
          [
            ['_fx_save', 128],
            ['_fx_s_r0', 4],
            ['_fx_s_i4', 4],
            ['_fx_s_l4', 4],
            ['_fx_ctx', 48],
            ['_fx_blk', 4],
            ['_fx_in', 12],
            ['_fx_scratch', 256],
            ['_fx_c', 16],
            ['_fx_diag', 32],
            ['_fxs_bus', 260],
            ['_fxs_gnew', 64],
            ['_fxs_prev', 256],
            ['_fxs_c', 16],
            ['_te_state_a', 16],
          ] as [string, number][]
        ).map(([n, size]) => [d[n]!, size]);
        allowed.push([FX_RETURN, 256]);
        allowed.push([TABLE, 5 * 96]); // the row's was word
        const bad: [string, number, string][] = [];
        for (const [pc, address, width] of b.stores) {
          const at = address >= SW_ALIAS_BASE ? address - SW_ALIAS_BASE : address;
          if (at < 0x200000) continue;
          if (!allowed.some(([a, n]) => a <= at && at + width <= a + n)) bad.push([at.toString(16), width, pc.toString(16)]);
        }
        expect(bad.slice(0, 4)).toEqual([]);
        expect(b.stores.length, 'the call stored nothing: the recording is off').toBeGreaterThan(0);
      },
      { recordStores: 'all' },
    ),
  );

  it('the cost of the send route in instructions', async () => {
    const steady = async (amounts: number[], mix = ONE): Promise<number> => {
      const rig = await open([sendRow({ amounts, mix })]);
      try {
        load(rig);
        await rig.sendBlock(undefined, 'friendly');
        await rig.sendBlock(undefined, 'friendly');
        return (await rig.sendBlock(undefined, 'friendly')).instructions;
      } finally {
        await rig.close();
      }
    };
    const none = await steady(zeros(TRACKS));
    const one = await steady(amountsOf(3, 100));
    const all16 = await steady(Array<number>(TRACKS).fill(100));
    const halfMix = await steady(Array<number>(TRACKS).fill(100), f2w(0.5));
    console.log(
      `one send effect (the half test effect, about 210): steady, all amounts 0 ${none}, one track ${one}, 16 tracks ${all16}; 16 tracks mix 0.5 ${halfMix} instructions per block`,
    );
    expect(none).toBeLessThan(1400);
    expect(one - none).toBeLessThan(170);
    expect(all16).toBeLessThan(3800);
    expect(halfMix).toBe(all16);
  });
});
