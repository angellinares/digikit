/**
 * A rig for dsp-voice-dn2-stereo.s: the stock DN2 1.12 DSP image with the stereo object linked beside it.
 *
 * The original firmware is the base core of the rig (`RigSetup.image`, read at run time from out/sections/dn2-1.12, never copied into a
 * fixture). The stock stages and sites are the image's: the wrappers call and jump to the real functions, and the pan hook is patched
 * into the real loop (an overlay of the 6 bytes at 0x1c9af6, the same bytes `patchkit` writes). The engine's memory is rebuilt
 * synthetically at test addresses: the engine's base (`ENGINE`), the table of voice buffers and the pan records behind it (`STEREO_TABLE`), the
 * table of plane pointers and the planes.
 *
 * Pointers are byte addresses (the engine's convention). Like the Python rig, a session is one core that keeps its memory between
 * calls: `poke` queues a DM write, `peek` and `words` answer from the core after the queued writes.
 *
 *     const rig = StereoRig.build();
 *     const session = await rig.session({ hooked: false });
 *     await buildEngine(session, left);
 *     const result = await runBlock(session);
 *     await session.close();
 */
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { f2w, w2f } from './context.ts';
import { OpQueue, realRig, type RigOpener } from './ops.ts';
import type { CallResult, RigPort } from './rig.ts';
import { ROOT } from './rig.ts';
import { DN2_112, stockHex } from './stock.ts';
import {
  SW_ALIAS_BASE,
  assemble,
  expandIncludes,
  link,
  sharcIncludeDirs,
  type Linked,
  type SharcObject,
} from './toolchain.ts';

export const STEREO_ADAPTER = path.join(ROOT, 'modules/core/adapters/dsp-voice');
export const STEREO_SOURCE = path.join(STEREO_ADAPTER, 'src/dsp/dsp-voice-dn2-stereo.s');

export const CODE = 0xb90000; // the stereo object (short-word address)
export const BACK = 0xb9f000; // the stand-in return address of a call
export const PAN_HOOK = 0x1c9af6;
export const PAN_NEXT = 0x1c9af9;
export const PAN_END = 0x1c9b30;
export const PRE_HOOK = 0x1c9aaf; // the first instruction after the fade loop
export const PRE_NEXT = 0x1c9ab2;
export const PAN_BODY = 0x1c9aea; // first instruction of the pan loop's body
export const BLOCK_FROM = 0x1c99de; // the block ramp, the fade, the metadata clear and the pan loop
export const BLOCK_TO = 0x1c9b33; // the instruction after the pan loop

/** stock functions (DN2 1.12), short-word addresses */
export const STOCK: Record<string, number> = {
  _gs_fo_stock: 0xb80c39,
  _gs_fo_reset_stock: 0xb80b93,
  _gs_uy_stock: 0xb81368,
  _gs_uy_reset_stock: 0xb8135a,
  _gs_gain_stock: 0xb80345,
  _gs_c5_stock: 0xb81526,
  _gs_held_stock: 0xb8265b,
  _gs_shaper_stock: 0x1cdb56,
  _gs_case1_stock: 0xb82342,
  _gs_case2_stock: 0xb81a16,
  _gs_case3_stock: 0xb80f2e,
  _gs_ring_stock: 0xb806f5,
  _gs_held_reset_stock: 0xb82440,
  _gs_case1_reset_stock: 0xb81d6d,
  _gs_case2_reset_stock: 0xb81c8d,
  _gs_case3_reset_stock: 0xb811f1,
  _gs_ring_reset_stock: 0xb805d8,
  _gs_case5_reset_stock: 0xb818c7,
};

// test addresses (L2 bytes)
export const ENGINE = 0x20110000; // `_gs_engine_base`: the engine's base address I9
export const STEREO_FRAME = 0x2010f000; // the callee frame of a wrapper call
export const CALLER = 0x2010f400; // the engine's frame: word -24 is the table of voice buffers
export const STEREO_STACK = 0x2010e000; // the engine's frame in the pan block runs
export const STEREO_TABLE = 0x20150000; // 16 pointers to the voice buffers; the pan records are at STEREO_TABLE + 0x354
export const STRUCT = 0x20151000; // word 3 points to the table of plane pointers
export const PLANE_TABLE = 0x20152000;
export const BUFFERS = 0x20170000; // voice buffer of slot v: BUFFERS + 0x80 v
export const PLANES = 0x20180000; // plane A of slot v: PLANES + 0x100 v, plane B + 0x80
export const DV = 0x200c0000; // stand-ins of the adapter's data: _dv_diag (128 bytes), _dv_c (64), _dv_in (96)
export const PRIVATE = 0x200d0000; // the stereo object's data

export const HEARTBEAT = 7; // the block counter the tests use
export const STUB = 0xb92000; // a stand-in for every stock function (stub: true): logs R4, R8, R12 and the call count at STUB_LOG, returns
export const STUB_LOG = 0x2010d000;

/** Every register the rig tests pin: R, I, M, L, B 0..15 are 0, and the context registers the engine keeps. */
export const REGS: Record<string, number> = (() => {
  const regs: Record<string, number> = {};
  for (const kind of 'RIMLB') for (let i = 0; i < 16; i++) regs[`${kind}${i}`] = 0;
  return { ...regs, M5: 0, M6: 1, M7: 0xffffffff, M13: 0, M14: 1, M15: 0xffffffff, MODE1: 0 };
})();

export const bits = f2w;
export const value = w2f;
export const f32 = Math.fround;
/** `n` copies of `word`. */
export const filled = (n: number, word: number): number[] => Array.from({ length: n }, () => word);
export const nanBlock = (n: number): number[] => filled(n, 0xffffffff);
const u32 = (n: number): number => n >>> 0;

/** The data runs of an object from `start` on, each section at its alignment (as `pk_sharc` places them); a data symbol is its run plus its offset. */
export function placeObjectData(
  obj: SharcObject,
  start: number,
): { data: Record<string, number>; inits: { address: number; bytes: Uint8Array; bss: boolean }[]; end: number } {
  let address = start;
  const data: Record<string, number> = {};
  const inits: { address: number; bytes: Uint8Array; bss: boolean }[] = [];
  for (const [section, { bytes, bss, align }] of Object.entries(obj.data)) {
    const a = Math.max(align ?? 4, 1);
    address = Math.ceil(address / a) * a;
    for (const [name, sym] of Object.entries(obj.dataSymbols))
      if (sym.section === section) data[name] = address + sym.offset;
    inits.push({ address, bytes, bss });
    address += bytes.length;
  }
  return { data, inits, end: address };
}

interface StereoBuild {
  objectData: Record<string, number>;
  inits: { address: number; bytes: Uint8Array; bss: boolean }[];
  dataEnd: number;
  dv: Record<string, number>;
  sym: Record<string, number>;
  code: Linked[];
  hook: Linked;
  pre: Linked;
  stubCode?: Linked;
  checked: number;
}

const builds = new Map<boolean, StereoBuild>();

function build(stub: boolean): StereoBuild {
  const known = builds.get(stub);
  if (known) return known;
  const dirs = [...sharcIncludeDirs(STEREO_ADAPTER), path.dirname(STEREO_SOURCE)];
  const obj = assemble(expandIncludes(readFileSync(STEREO_SOURCE, 'utf8'), dirs));
  const { data, inits, end: address } = placeObjectData(obj, PRIVATE);
  const dv = { _dv_diag: DV, _dv_c: DV + 0x100, _dv_in: DV + 0x200 };
  const stock = stub ? Object.fromEntries(Object.keys(STOCK).map((n) => [n, STUB])) : STOCK;
  const externs = {
    ...data,
    ...dv,
    ...stock,
    _gs_engine_base: ENGINE,
    _gs_pan_cont: PAN_NEXT,
    _gs_pan_end: PAN_END,
    _gs_pre_cont: PRE_NEXT,
  };
  const linked = link(obj, CODE, externs);
  let stubCode: Linked | undefined;
  if (stub) {
    stubCode = link(
      assemble(`.SECTION/PM seg_pmco;
.GLOBAL _stub;
_stub:
    DM(${STUB_LOG}) = R4;
    DM(${STUB_LOG + 4}) = R8;
    DM(${STUB_LOG + 8}) = R12;
    R0 = DM(${STUB_LOG + 12});
    R1 = 1;
    R0 = R0 + R1;
    DM(${STUB_LOG + 12}) = R0;
    I12 = DM(-1, I6);
    NOP;
    NOP;
    NOP;
    JUMP (M14, I12) (DB);
    NOP;
    RFRAME;
`),
      STUB,
    );
  }
  // the pan hook, as patchkit writes it: JUMP _gs_slot over the instruction at 0x1c9af6
  const hook = link(assemble('.SECTION/PM seg_pmco;\n.GLOBAL _site;\n_site: JUMP _gs_slot;'), PAN_HOOK, {
    _gs_slot: linked.symbols._gs_slot!,
  });
  if (hook.codeBoot.length !== 12) throw new Error(`the pan hook is ${hook.codeBoot.length / 2} bytes, not 6`);
  const pre = link(assemble('.SECTION/PM seg_pmco;\n.GLOBAL _site;\n_site: JUMP _gs_prepan;'), PRE_HOOK, {
    _gs_prepan: linked.symbols._gs_prepan!,
  });
  if (pre.codeBoot.length !== 12) throw new Error(`the pre-pan hook is ${pre.codeBoot.length / 2} bytes, not 6`);
  const b: StereoBuild = {
    objectData: data,
    inits,
    dataEnd: address,
    dv,
    sym: linked.symbols,
    code: [linked],
    hook,
    pre,
    stubCode,
    checked: linked.checked ?? 0,
  };
  builds.set(stub, b);
  return b;
}

export interface SessionOptions {
  /** the pan hook and the pre-pan hook patched into the image (default true) */
  hooked?: boolean;
  /** the stereo object's BSS holds NaN words, as unwritten memory would (default true) */
  poison?: boolean;
}

export interface RawCallOptions {
  maxSteps?: number;
  readRegs?: readonly string[];
  reads?: [address: number, count: number][];
  /** pcs that end the call (default: BACK) */
  breaks?: number[];
  /** continue from where the last call stopped */
  resume?: boolean;
  /** 80-bit multiplier registers set before the call, `[mask, bits]` as hex */
  specials?: Record<string, [mask: string, bits: string]>;
  readSpecials?: string[];
  /** count the instructions executed at each pc (`CallResult.profile`) */
  profile?: boolean;
}

export class StereoRig {
  /** data symbol of the stereo object -> byte address */
  readonly data: Record<string, number>;
  /** code symbol -> short-word address */
  readonly sym: Record<string, number>;
  readonly dataEnd: number;
  readonly stub: boolean;
  /** instructions the link re-decoded */
  readonly checked: number;
  private readonly b: StereoBuild;
  private readonly open: RigOpener;

  private constructor(stub: boolean, open: RigOpener) {
    this.b = build(stub);
    this.data = this.b.objectData;
    this.sym = this.b.sym;
    this.dataEnd = this.b.dataEnd;
    this.stub = stub;
    this.checked = this.b.checked;
    this.open = open;
  }

  /** Assembles and links once per process; `session()` makes a fresh core. `stub`: every stock function is the logging stand-in. */
  static build(o: { stub?: boolean; open?: RigOpener } = {}): StereoRig {
    return new StereoRig(o.stub ?? false, o.open ?? realRig);
  }

  async session(o: SessionOptions = {}): Promise<StereoSession> {
    const hooked = o.hooked ?? true;
    const poison = o.poison ?? true;
    const linked = hooked ? [...this.b.code, this.b.hook, this.b.pre] : [...this.b.code];
    if (this.b.stubCode) linked.push(this.b.stubCode);
    const pokes: [number, number][] = [];
    for (let off = 0; off < 128; off += 4) pokes.push([DV + off, 0]);
    pokes[0] = [DV, HEARTBEAT];
    for (let off = 0; off < 16; off += 4) pokes.push([STUB_LOG + off, 0]);
    return openImageSession({ code: linked, inits: this.b.inits, poison, pokes, open: this.open });
  }
}

export interface ImageSessionOptions {
  /** the objects linked over the stock image */
  code: Linked[];
  /** the data runs of the objects (`placeObjectData`): every word is poked, the BSS as NaN words when `poison` */
  inits?: { address: number; bytes: Uint8Array; bss: boolean }[];
  poison?: boolean;
  /** more DM words, poked after the data runs */
  pokes?: [address: number, word: number][];
  /** `[first pc, end pc]` ranges whose stores the calls report (`CallResult.stores`) */
  watch?: [number, number][];
  /** the pc a call returns to (default BACK) */
  returnSw?: number;
  open?: RigOpener;
}

/** A core with the stock DN2 1.12 image and `code` over it. */
export async function openImageSession(o: ImageSessionOptions): Promise<StereoSession> {
  const q = new OpQueue();
  for (const { address, bytes, bss } of o.inits ?? []) {
    for (let off = 0; off < bytes.length; off += 4) {
      let word = 0;
      for (let k = Math.min(off + 4, bytes.length) - 1; k >= off; k--) word = (word * 256 + bytes[k]!) >>> 0;
      q.poke(address + off, bss && (o.poison ?? true) ? 0xffffffff : word);
    }
  }
  for (const [a, w] of o.pokes ?? []) q.poke(a, w);
  const { port } = await (o.open ?? realRig)({
    return_sw: o.returnSw ?? BACK,
    breaks: [],
    ...(o.watch ? { watch: o.watch } : {}),
    image: stockHex(DN2_112),
    code: o.code.map((l) => ({ address: SW_ALIAS_BASE + 2 * l.baseSw, hex: l.codeBoot })),
    ops: q.take(),
  });
  return new StereoSession(port);
}

/** One core: the stock image, the stereo object and the DM the test wrote. */
export class StereoSession {
  private readonly q = new OpQueue();
  private closed = false;

  private readonly port: RigPort;

  constructor(port: RigPort) {
    this.port = port;
  }

  async close(): Promise<void> {
    if (this.closed) return;
    this.closed = true;
    await this.port.close();
  }

  /** Write a DM word (the address as the Python rig passes it: a byte address of L2 or DDR, or a core address). */
  poke(address: number, word: number): void {
    this.q.poke(address, u32(word));
  }

  pokeWords(address: number, values: readonly number[]): void {
    values.forEach((v, i) => this.q.poke(address + 4 * i, u32(v)));
  }

  /** The words at `address` now; null for a word that nothing wrote. */
  async peekWords(address: number, count: number): Promise<(number | null)[]> {
    return (await this.peekMany([[address, count]]))[0]!;
  }

  async peek(address: number): Promise<number | null> {
    return (await this.peekWords(address, 1))[0]!;
  }

  /** `count` words at byte address `address` as numbers (a word that nothing wrote reads as null, which a comparison sees). */
  async words(address: number, count: number): Promise<number[]> {
    return (await this.peekWords(address, count)) as number[];
  }

  /** Several ranges `[byte address, word count]` in one round trip. The read is a zero-step resume: a call that stopped at a break stays resumable. */
  async peekMany(ranges: [address: number, count: number][]): Promise<(number | null)[][]> {
    // an address this large is a byte address (a count would step by one byte): every word is its own read
    const reads: [number, number][] = [];
    for (const [a, n] of ranges) for (let i = 0; i < n; i++) reads.push([a + 4 * i, 1]);
    const r = await this.run({ pc: BACK, regs: {}, maxSteps: 0, reads, breaks: [BACK], resume: true });
    let k = 0;
    return ranges.map(([, n]) => Array.from({ length: n }, () => r.reads[k++]![0]!));
  }

  /** The stock image's bytes at loader byte addresses (before the stereo object and the hooks went over them). */
  async stock(ranges: [address: number, count: number][]): Promise<(string | null)[]> {
    const r = await this.run({ pc: BACK, regs: {}, maxSteps: 0, stock_reads: ranges, breaks: [BACK], resume: true });
    return r.stock ?? [];
  }

  private async run(call: {
    pc: number;
    regs: Record<string, number>;
    maxSteps: number;
    reads?: [number, number][];
    read_regs?: string[];
    stock_reads?: [number, number][];
    breaks?: number[];
    resume?: boolean;
    specials?: Record<string, [string, string]>;
    read_specials?: string[];
    profile?: boolean;
  }): Promise<CallResult> {
    this.q.push({
      call: {
        pc: call.pc,
        regs: Object.fromEntries(Object.entries(call.regs).map(([n, v]) => [n, u32(v)])),
        max_steps: call.maxSteps,
        reads: call.reads ?? [],
        read_regs: call.read_regs ?? [],
        ...(call.stock_reads ? { stock_reads: call.stock_reads } : {}),
        ...(call.breaks ? { breaks: call.breaks } : {}),
        ...(call.resume ? { resume: true } : {}),
        ...(call.specials ? { specials: call.specials } : {}),
        ...(call.read_specials ? { read_specials: call.read_specials } : {}),
        ...(call.profile ? { profile: true } : {}),
      },
    });
    const results = await this.port.run(this.q.take());
    return results.at(-1)!;
  }

  /** `CallSession.call`: the registers named are set, every other register stays as the last call left it; ends at a break or BACK. */
  async raw(pc: number, regs: Record<string, number>, o: RawCallOptions = {}): Promise<CallResult> {
    return this.run({
      pc,
      regs,
      maxSteps: o.maxSteps ?? 100_000,
      reads: o.reads,
      read_regs: o.readRegs ? [...o.readRegs] : [],
      breaks: o.breaks ?? [BACK],
      resume: o.resume,
      specials: o.specials,
      read_specials: o.readSpecials,
      profile: o.profile,
    });
  }
}

// ---- the engine's data for the pan block and the wrapper calls ----

export interface EngineOptions {
  /** ramp[v] = [start, step] floats of the block ramp (default 1.0, 0.0) */
  ramp?: Record<number, [number, number]>;
  /** the slots whose two fade flags are nonzero (byte v of the array at engine + 0x138fc, word 5 of the gain record) */
  fade?: Set<number>;
  /** pan[v] = [gA, stepA, gB, stepB] floats of the pan record */
  pan?: Record<number, [number, number, number, number]>;
  /** gain_words[v]: the 11 words of the gain record (the fade's second flag is word 5) */
  gainWords?: Record<number, number[]>;
}

/** The engine's memory behind the pan block (BLOCK_FROM to BLOCK_TO) for 16 slots; left[v]: the 32 float bits of the voice buffer of slot v. */
export function buildEngine(s: StereoSession, left: Record<number, number[]>, o: EngineOptions = {}): void {
  const fade = o.fade ?? new Set<number>();
  s.poke(STEREO_STACK - 24 * 4, STEREO_TABLE);
  s.poke(STEREO_STACK - 30 * 4, STRUCT);
  s.poke(STRUCT + 12, PLANE_TABLE);
  s.poke(STEREO_STACK, CALLER);
  for (let v = 0; v < 16; v++) {
    s.poke(STEREO_TABLE + 4 * v, BUFFERS + 0x80 * v);
    s.poke(PLANE_TABLE + 8 * v, PLANES + 0x100 * v);
    s.poke(PLANE_TABLE + 8 * v + 4, PLANES + 0x100 * v + 0x80);
    s.pokeWords(BUFFERS + 0x80 * v, left[v]!);
    s.pokeWords(PLANES + 0x100 * v, nanBlock(64));
    const record = STEREO_TABLE + 0x354 + 28 * v;
    s.pokeWords(record, filled(7, 0));
    const [ga, stepA, gb, stepB] = o.pan?.[v] ?? [0.5 + v / 64, 0.0007 * (v + 1), 0.25 + v / 128, -0.0005 * (v + 1)];
    s.poke(record, bits(ga));
    s.poke(record + 8, bits(stepA));
    s.poke(record + 0x70 * 4, bits(gb));
    s.poke(record + 0x72 * 4, bits(stepB));
    const [start, step] = o.ramp?.[v] ?? [1.0, 0.0];
    s.pokeWords(ENGINE + 0x1395c + 28 * v, filled(7, 0));
    s.poke(ENGINE + 0x1395c + 28 * v, bits(start));
    s.poke(ENGINE + 0x1395c + 28 * v + 8, bits(step));
    s.pokeWords(ENGINE + 0x1351c - 20 + 44 * v, o.gainWords?.[v] ?? filled(11, 0));
    if (fade.has(v)) s.poke(ENGINE + 0x1351c + 44 * v, 1);
  }
  setFadeFlags(s, fade);
}

/** The byte array engine + 0x138fc (one byte per slot): 1 for the slots, 0 for the others. */
export function setFadeFlags(s: StereoSession, slots: Set<number>): void {
  for (let word = 0; word < 4; word++) {
    let w = 0;
    for (let k = 0; k < 4; k++) if (slots.has(4 * word + k)) w |= 1 << (8 * k);
    s.poke(ENGINE + 0x138fc + 4 * word, w);
  }
}

export function engineRegs(extra: Record<string, number> = {}): Record<string, number> {
  return { ...REGS, I6: STEREO_STACK, I7: STEREO_STACK - 0x100, I9: ENGINE, I4: ENGINE, R9: 32, R11: 0, ...extra };
}

/** Run the pan block from BLOCK_FROM to `stop`; the call must stop there. */
export async function runBlock(s: StereoSession, stop = BLOCK_TO, steps = 60000): Promise<CallResult> {
  const result = await s.raw(BLOCK_FROM, engineRegs(), { maxSteps: steps, breaks: [BACK, stop] });
  if (result.halt !== 'breakpoint' || result.final_pc !== stop)
    throw new Error(`the block did not stop at ${stop.toString(16)}: ${JSON.stringify(result)}`);
  return result;
}

/** The two planes of every slot (A, B) as 32 words each. */
export async function planes(s: StereoSession): Promise<[number[], number[]][]> {
  const ranges: [number, number][] = [];
  for (let v = 0; v < 16; v++) ranges.push([PLANES + 0x100 * v, 32], [PLANES + 0x100 * v + 0x80, 32]);
  const rows = (await s.peekMany(ranges)) as number[][];
  return Array.from({ length: 16 }, (_, v) => [rows[2 * v]!, rows[2 * v + 1]!]);
}

export async function buffers(s: StereoSession): Promise<number[][]> {
  return (await s.peekMany(
    Array.from({ length: 16 }, (_, v) => [BUFFERS + 0x80 * v, 32] as [number, number]),
  )) as number[][];
}

// ---- a wrapper call the way the engine makes a linked call ----

export const CALLEE_SAVED = [
  'R3',
  'R5',
  'R6',
  'R7',
  'R9',
  'R10',
  'R11',
  'R13',
  'R14',
  'R15',
  'I0',
  'I1',
  'I2',
  'I3',
  'I5',
  'M1',
  'M2',
  'M3',
] as const;

/** A fresh core with the engine's caller frame and the table of voice buffers, hooks as asked. */
export async function newSession(rig: StereoRig, hooked = false): Promise<StereoSession> {
  const s = await rig.session({ hooked });
  s.poke(CALLER - 24 * 4, STEREO_TABLE);
  for (let v = 0; v < 16; v++) s.poke(STEREO_TABLE + 4 * v, BUFFERS + 0x80 * v);
  return s;
}

export function setStack(s: StereoSession, args: readonly number[]): void {
  s.poke(STEREO_FRAME, CALLER);
  s.poke(STEREO_FRAME - 4, BACK - 1);
  args.forEach((a, i) => s.poke(STEREO_FRAME + 4 * (i + 1), a));
}

export interface WrapperCall {
  instructions: number;
  regs: Record<string, number | null>;
}

/** One call of a stock function or a twin: R4 = record, R8 = input, R12 = output, the arguments on the stack. Throws when a callee-saved register changes. */
export async function call(
  s: StereoSession,
  entryPc: number,
  rec: number,
  inp: number,
  out: number,
  args: readonly number[],
  o: { sentinels?: boolean; regs?: Record<string, number> } = {},
): Promise<WrapperCall> {
  const sentinels = o.sentinels ?? true;
  setStack(s, args);
  const r: Record<string, number> = {
    ...REGS,
    I6: STEREO_FRAME,
    I7: STEREO_FRAME - 8,
    R2: CALLER,
    R4: rec,
    R8: inp,
    R12: out,
  };
  if (sentinels)
    CALLEE_SAVED.forEach((n, i) => {
      r[n] = n.startsWith('I') ? 0x20000100 + 16 * i : 0x3000 + i;
    });
  Object.assign(r, o.regs);
  const result = await s.raw(entryPc, r, { maxSteps: 40000, readRegs: CALLEE_SAVED });
  if (result.halt !== 'breakpoint' || result.final_pc !== BACK)
    throw new Error(`call did not return: ${JSON.stringify(result)}`);
  if (sentinels)
    for (const n of CALLEE_SAVED)
      if (u32(result.regs[n] ?? -1) !== u32(r[n]!))
        throw new Error(`${n} is callee-saved: ${result.regs[n]} for ${r[n]}`);
  return { instructions: result.instructions, regs: result.regs };
}
