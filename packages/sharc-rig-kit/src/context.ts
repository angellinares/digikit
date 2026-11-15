/**
 * The stand-in addresses of the rigs and the context registers (port of tests/support/dsp_voice_rig.py: the constants, FRIENDLY and
 * `draw_context`).
 *
 * The firmware restores M5, M6, M13, M15, the L registers and MODE1 per DSP context (docs/findings 45), so the values at the hook
 * are the context's, not constants. The rigs draw them (never the friendly 0 / 1 / 0 / -1) and the code under test must not
 * depend on them: every context register comes back unchanged. M7 = -1 and M14 = 1 stay, as the stock epilogue and every stack
 * operation of the caller use them.
 */
import type { PyRandom } from './prng.ts';

export const STOCK_BP = 0xb9f000; // stands in for the stock renderer's second instruction
export const RETURN_SW = 0xb9f100; // the caller's return point
export const UNPACK_BP = 0xb9f010; // stands in for the stock per-track unpack
// byte addresses of the stand-ins (L2), one 4 KiB page each
export const TYPE_CACHE = 0x20091000;
export const FRAME = 0x20092000;
export const RECORDS = 0x20093000;
export const OUT = 0x20098000;
export const TABLE = 0x2009a000;
export const STACK = 0x2009c000;
export const DIAG = 0x2009d000;
export const PARAM_REC = 0x2506ec; // DT2 1.17 param record base (the stock literal)
export const RELEASE = DIAG + 0x800; // release bytes: byte v is 1 for about a block after voice v's note-off
export const REC = 0x1d8; // bytes of a voice record

/** Byte address to normal-word address. */
export const nw = (byte: number): number => Math.floor(byte / 4);

export const FRIENDLY: Record<string, number> = {
  M5: 0,
  M6: 1,
  M7: -1,
  M13: 0,
  M14: 1,
  M15: -1,
  L0: 0,
  L1: 0,
  L2: 0,
  L3: 0,
  L4: 0,
  L5: 0,
  MODE1: 0x1011800,
};

export const M6_HOSTILE = [0x1b8, 4, 0, -1];
export const M13_HOSTILE = [4, -2, 0x10];
const ROUND_BITS = 0x1011800; // the startup MODE1: IRPTEN, NESTM, RND32, CBUFEN
const MODE1_CHOICES = [
  ROUND_BITS,
  ROUND_BITS | 0x8000, // TRUNCATE
  ROUND_BITS | 0x2000, // ALUSAT
  ROUND_BITS & ~0x10000, // RND32 clear
  ROUND_BITS | 0x8000 | 0x2000,
];

export const CONTEXT_REGS = [
  'M5',
  'M6',
  'M7',
  'M13',
  'M14',
  'M15',
  'L0',
  'L1',
  'L2',
  'L3',
  'L4',
  'L5',
  'MODE1',
] as const;

export type Context = Record<string, number>;

/** One context as the firmware could leave it: every pinned register hostile with some probability. */
export function drawContext(rng: PyRandom): Context {
  const ctx = { ...FRIENDLY };
  ctx.M5 = rng.choice([0, 4, 0x1b8, -2, 0x10, 7]);
  ctx.M6 = rng.choice([1, ...M6_HOSTILE, -2, 7]);
  ctx.M13 = rng.choice([0, ...M13_HOSTILE, 0x1b8]);
  ctx.M15 = rng.choice([-1, 0, 4, 3]);
  for (let n = 0; n < 6; n++) ctx[`L${n}`] = rng.choice([0, 0, 5, 33, 0x1fd]);
  ctx.MODE1 = rng.choice(MODE1_CHOICES);
  return ctx;
}

/** `ctx` as a block takes it: undefined draws a hostile context, "friendly" the constants, an object is taken as given (missing names friendly). */
export function resolveContext(ctx: Context | 'friendly' | undefined, rng: PyRandom): Context {
  if (ctx === undefined) return drawContext(rng);
  if (ctx === 'friendly') return { ...FRIENDLY };
  return { ...FRIENDLY, ...ctx };
}

const u32 = (n: number): number => n >>> 0;

/** The context registers that did not come back as they went in; throws when the rig did not report one (`finalPc` names the call). */
export function contextBad(regs: Record<string, number | null>, ctx: Context, finalPc?: number): string[] {
  return CONTEXT_REGS.filter((n) => {
    const v = regs[n];
    if (v == null) throw new Error(`the rig did not report register ${n} (call at pc ${finalPc})`);
    return u32(v) !== u32(ctx[n]!);
  });
}

export const f2w = (x: number): number => {
  const view = new DataView(new ArrayBuffer(4));
  view.setFloat32(0, x, true);
  return view.getUint32(0, true);
};

export const w2f = (w: number): number => {
  const view = new DataView(new ArrayBuffer(4));
  view.setUint32(0, w >>> 0, true);
  return view.getFloat32(0, true);
};
