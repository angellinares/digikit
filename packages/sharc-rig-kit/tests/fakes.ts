import { STOCK_BP, RETURN_SW } from '../src/context.ts';
import type { FxBuilt, MachineBuilt } from '../src/place.ts';
import type { CallResult, Op, RigPort, RigSetup } from '../src/rig.ts';
import type { Frame, MachineSpec, Param } from '../src/spec.ts';

export const param = (id: string, knob: number, lo = 0, hi = 0x7f00, dflt = 0x4000): Param => ({
  id,
  knob,
  lo,
  hi,
  default: dflt,
  stock: false,
  latchedWord: null,
});

export const FRAME_LAYOUT: Frame = { base: 0x6d, stride: 48, group: 4, gap: 2 };

export function spec(device: 'dt2' | 'dn2', params: Param[] = [param('a', 1), param('b', 2)]): MachineSpec {
  return {
    module: 'test/fake',
    device,
    moddir: '/nowhere',
    sources: ['/nowhere/src/dsp/fake.s'],
    entry: '_f_render',
    machineType: 20,
    params,
    frame: FRAME_LAYOUT,
    noteOn: null,
    desc: null,
    fixtures: [],
    sampleRecords: null,
  };
}

/** Code and symbols as the build would place them, without a toolchain. The module's code is short words 0x1000 to 0x1100. */
export function built(): MachineBuilt {
  return {
    linked: [{ baseSw: 0xb90000, codeBoot: '0000', symbols: {} }],
    data: { _dv_mask: 0x200a0000 },
    inits: [{ address: 0x200a0010, bytes: new Uint8Array(8), bss: true }],
    symbols: { _dv_entry: 0xb90000, _f_render: 0x1000 },
    state: 0,
    stride: 0,
    clear: 0,
    desc: [],
    moduleCode: [[0x1000, 0x1100]],
    moduleData: [{ address: 0x200a0010, bytes: 8 }],
    wrapperData: [],
  };
}

type Reply = (call: number, ops: Op[]) => Partial<CallResult>;

export interface FakeRig {
  setups: RigSetup[];
  /** the ops of every batch, in order */
  batches: Op[][];
  open: (setup: RigSetup) => Promise<{ port: RigPort; first: CallResult[] }>;
  closed: () => boolean;
}

/**
 * A transport that answers every call with an ordinary result (returns to the stop address, echoes the registers it was asked to read,
 * reads every word as 0) with the fields that `reply` names replaced.
 */
export function fakeRig(device: 'dt2' | 'dn2', reply: Reply = () => ({})): FakeRig {
  const rig: FakeRig = {
    setups: [],
    batches: [],
    closed: () => closed,
    open: async (setup) => {
      rig.setups.push(setup);
      let calls = 0;
      const port: RigPort = {
        async run(ops) {
          rig.batches.push(ops);
          const out: CallResult[] = [];
          for (const op of ops) {
            if (!('call' in op)) continue;
            const c = op.call;
            const regs: Record<string, number | null> = {};
            for (const n of c.read_regs) regs[n] = c.regs[n] ?? 0;
            out.push({
              instructions: 100,
              halt: 'breakpoint',
              final_pc: device === 'dt2' ? RETURN_SW : STOCK_BP,
              regs,
              reads: c.reads.map(([, n]) => Array<number | null>(n).fill(0)),
              stores: [],
              ...reply(calls++, ops),
            });
          }
          return out;
        },
        async close() {
          closed = true;
        },
      };
      return { port, first: [] };
    },
  };
  let closed = false;
  return rig;
}

/** The code and symbols of an effect chain as the build would place them: the module's code is short words 0x1000 to 0x1100. */
export function fxBuilt(): FxBuilt {
  const desc = new Uint8Array(24);
  new DataView(desc.buffer).setUint32(0, 0x444b4658, true);
  new DataView(desc.buffer).setUint32(4, 1, true);
  new DataView(desc.buffer).setUint32(8, 3, true);
  return {
    linked: [{ baseSw: 0xb90000, codeBoot: '0000', symbols: {} }],
    data: {
      _te_desc: 0x200a0000,
      _te_state_a: 0x200a0100,
      _fx_auto_state1: 0x200a0200,
      _fx_scratch: 0x200a0300,
      _fx_save: 0x200a0400,
      _fx_in: 0x200a0500,
    },
    inits: [
      { address: 0x200a0000, bytes: desc, bss: false },
      { address: 0x200a0100, bytes: new Uint8Array(8), bss: true },
    ],
    symbols: { _fx_entry: 0xb90000, _te_half: 0x1000 },
    moduleCode: [[0x1000, 0x1100]],
    moduleData: [{ address: 0x200a0100, bytes: 8 }],
  };
}
