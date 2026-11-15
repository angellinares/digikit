/**
 * A rig for core/dsp-voice: the wrapper and one routine, called the way the engine calls them (port of the `Rig` of
 * tests/support/dsp_voice_rig.py and of the `CrumbRig` of test_dsp_voice.py).
 *
 *     const rig = VoiceRig.sine();            // the wrapper and tests/support/raw_sine.s
 *     rig.setType(0, VOICE_TYPE); rig.setNote(0, 60); rig.recWord(0, 0x1b8, 1);
 *     const out = await rig.call(0);          // out.reads.out: the 32 words of voice 0's block; out.regs, out.in, out.halt
 *
 * Like the Python rig, every call runs on a fresh core: the DM the test built in `dm` (the stand-ins and everything the test wrote) is
 * poked, the call runs, and what the call left comes back in the reads the test asked for. A test that continues a state writes the
 * words it read back into `dm`. (The machine rig, in contrast, keeps one core for all blocks.)
 */
import { readFileSync } from 'node:fs';
import path from 'node:path';
import {
  CONTEXT_REGS,
  DIAG,
  FRAME,
  FRIENDLY,
  OUT,
  PARAM_REC,
  REC,
  RECORDS,
  RELEASE,
  RETURN_SW,
  STACK,
  STOCK_BP,
  TABLE,
  TYPE_CACHE,
  UNPACK_BP,
  drawContext,
  nw,
  type Context,
} from './context.ts';
import { realRig, type RigOpener } from './ops.ts';
import { PyRandom } from './prng.ts';
import { ROOT, type CallResult } from './rig.ts';
import {
  SW_ALIAS_BASE,
  applyConditionals,
  assemble,
  expandIncludes,
  link,
  sharcIncludeDirs,
  type Linked,
} from './toolchain.ts';

export const VOICE_CORE = path.join(ROOT, 'modules/core/adapters/dsp-voice/src/dsp/dsp-voice.s');
/** a sine routine of the raw (no contract) form */
export const VOICE_SINE = path.join(ROOT, 'tests/support/raw_sine.s');
export const CORE_BASE = 0xb90000;
export const SINE_BASE = 0xb92000;
export const VOICE_STATE = 0x2009b000;
export const SV_L4 = DIAG + 0x100; // the wrapper's data symbols (the build places them in free L2)
export const SV3 = DIAG + 0x104;
export const SV5 = DIAG + 0x108;
export const BCW = DIAG + 0x10c;
export const REC_PTRS = DIAG + 0x110;
export const STEP_WORD = DIAG + 0x120; // a stand-in for the module step word `_dk_step`; its address is `_dv_rec[2]`
export const MASK = DIAG + 0x11c; // `_dv_mask`: the registered types, rebuilt by voice 0 from the table
export const GUARD = DIAG + 0x200; // the guard's scalars and arrays (_dv_g, 128 bytes)
/** the contract adapter's data: zero, and not used by a row of the old form */
export const ADAPTER: Record<string, number> = {
  _dv_in: DIAG + 0x300,
  _dv_params: DIAG + 0x320,
  _dv_blk: DIAG + 0x360,
  _dv_c: DIAG + 0x3a0,
  _dv_slots: DIAG + 0x3e0,
  _dv_st: DIAG + 0x400, // 16 words: the per-track stereo flag (machine contract 1.2)
  _dv_call_params: DIAG + 0x440, // 8 words: the merged callback words
  _dv_trig_params: DIAG + 0x500, // 128 words: per voice/slot latch snapshots
  _dv_gate: DIAG + 0x700, // 16 words: the per-track gate (machine contract 1.6)
};
export const VOICE_ROW = 24; // words of a registry row
export const RING_A = 0x200a0000; // stand-ins for the two reply records (the breadcrumbs are written into both)
export const RING_B = 0x200a2000;
export const VOICE_TYPE = 8;

export function coreExterns(): Record<string, number> {
  return {
    _fw_type_cache: TYPE_CACHE,
    _fw_render_next: STOCK_BP,
    _fw_frame: FRAME,
    _fw_param_rec: PARAM_REC,
    _fw_release_latch: RELEASE,
    _fw_unpack: UNPACK_BP,
    _dv_table: TABLE,
    _dv_diag: DIAG,
    _dv_sv_l4: SV_L4,
    _dv_sv3: SV3,
    _dv_sv5: SV5,
    _dv_bcw: BCW,
    _dv_rec: REC_PTRS,
    _dv_mask: MASK,
    _dv_g: GUARD,
    ...ADAPTER,
  };
}

/** The routine the wrapper calls: its assembler text, where it goes, the symbols it needs of the wrapper and its entry. */
export interface VoiceRoutine {
  text: string;
  base: number;
  /** stand-in symbols (the symbols of the wrapper are passed in) */
  externs: (core: Record<string, number>) => Record<string, number>;
  symbol: string;
}

export function sineRoutine(): VoiceRoutine {
  return {
    text: readFileSync(VOICE_SINE, 'utf8'),
    base: SINE_BASE,
    externs: (core) => ({ _dv_exp2: core._dv_exp2!, _dv_frame_hw: core._dv_frame_hw!, _sv_state: VOICE_STATE }),
    symbol: '_sv_render',
  };
}

interface VoiceBuild {
  core: Linked;
  routine: Linked;
  renderPc: number;
}

const builds = new Map<string, VoiceBuild>();

function build(defines: readonly string[], r: VoiceRoutine): VoiceBuild {
  const key = JSON.stringify([[...defines].sort(), r.text, r.base, r.symbol]);
  let b = builds.get(key);
  if (!b) {
    const moddir = path.resolve(path.dirname(VOICE_CORE), '../..');
    const text = applyConditionals(readFileSync(VOICE_CORE, 'utf8'), [...defines]);
    const coreObj = assemble(expandIncludes(text, [...sharcIncludeDirs(moddir), path.dirname(VOICE_CORE)]));
    const core = link(coreObj, CORE_BASE, coreExterns());
    const routine = link(assemble(r.text), r.base, r.externs(core.symbols));
    b = { core, routine, renderPc: routine.symbols[r.symbol]! };
    builds.set(key, b);
  }
  return b;
}

export interface VoiceCall {
  halt: string;
  finalPc: number;
  instructions: number;
  /** the registers the call left (a register that is not a constant reads 0) */
  regs: Record<string, number>;
  /** the registers the call was given */
  in: Record<string, number>;
  /** the reads, by name (a word that nothing wrote is null); `out` and `table` are always there */
  reads: Record<string, (number | null)[]>;
}

export interface VoiceOptions {
  seed?: number;
  defines?: readonly string[];
  routine?: VoiceRoutine;
  open?: RigOpener;
}

const READ_REGS = [
  'R4',
  'R8',
  'I6',
  'I7',
  'I4',
  ...CONTEXT_REGS,
  'I8',
  'I9',
  'M8',
  'R3',
  'R5',
  'R15',
  'I0',
  'I5',
  'M1',
  'M3',
];

export interface CallOptions {
  regs?: Record<string, number>;
  /** name -> [normal-word address, count] */
  reads?: Record<string, [number, number]>;
  maxSteps?: number;
  ctx?: Context | 'friendly';
  breaks?: number[];
  /** `_dv_mask` as an earlier call of voice 0 left it (default: what voice 0 would build from the table now) */
  mask?: number;
}

const u32 = (n: number): number => n >>> 0;

export class VoiceRig {
  readonly coreSymbols: Record<string, number>;
  readonly coreChecked: number;
  readonly routineChecked: number;
  /** the DM the next call starts from: normal-word address -> word */
  dm = new Map<number, number>();
  lastCtx: Context = {};
  private readonly ctxRng: PyRandom;
  private readonly b: VoiceBuild;
  private readonly open: RigOpener;

  constructor(o: VoiceOptions = {}) {
    this.ctxRng = new PyRandom(o.seed ?? 0xd1617);
    this.b = build(o.defines ?? [], o.routine ?? sineRoutine());
    this.coreSymbols = this.b.core.symbols;
    this.coreChecked = this.b.core.checked ?? 0;
    this.routineChecked = this.b.routine.checked ?? 0;
    this.open = o.open ?? realRig;
    this.reset();
  }

  /** The wrapper with the raw sine routine. */
  static sine(o: Omit<VoiceOptions, 'routine'> = {}): VoiceRig {
    return new VoiceRig(o);
  }

  reset(): void {
    this.dm = new Map();
    const zero = (byte: number, n: number) => {
      for (let k = 0; k < n; k++) this.dm.set(nw(byte) + k, 0);
    };
    zero(TABLE, 17 * VOICE_ROW);
    zero(VOICE_STATE, 32);
    zero(TYPE_CACHE, 8);
    zero(FRAME, 256);
    zero(OUT, 32 * 32); // the voice buffers hold the last block (a routine that writes none leaves them)
    zero(RECORDS, Math.floor((32 * REC) / 4));
    this.dm.set(nw(TABLE), VOICE_TYPE);
    this.dm.set(nw(TABLE) + 1, this.b.renderPc);
    zero(REC_PTRS, 3); // no reply records yet (core/dsp-link has not run): the breadcrumbs stay in _dv_diag
    zero(DIAG, 32);
    zero(GUARD, 32);
    zero(ADAPTER._dv_st!, 16); // no track has written R (machine contract 1.2)
    [1, 2, 4, 5, 255, 255, 255, 255].forEach((w, k) => this.dm.set(nw(ADAPTER._dv_slots!) + k, w)); // the DT2 knob positions
  }

  word(address: number): number {
    return this.dm.get(address) ?? 0;
  }

  poke(address: number, value: number): void {
    this.dm.set(address, u32(value));
  }

  /** The mask voice 0 builds from the rows now in the table (type t sets bit t, 31 or more bit 31; free rows and the zero row's end are no types). */
  tableMask(): number {
    let mask = 0;
    for (let row = 0; row < 17; row++) {
      const t = this.word(nw(TABLE) + row * VOICE_ROW);
      if (t === 0) break;
      if (t !== 0xffffffff) mask |= 1 << Math.min(t, 31);
    }
    return u32(mask);
  }

  /** The two reply records core/dsp-link's block tail would have registered. */
  setRings(): void {
    this.dm.set(nw(REC_PTRS), RING_A);
    this.dm.set(nw(REC_PTRS) + 1, RING_B);
    this.dm.set(nw(REC_PTRS) + 2, STEP_WORD);
  }

  /** Halfword K of the frame copy. */
  hw(k: number, v: number): void {
    const word = nw(FRAME) + Math.floor(k / 2);
    const sh = 16 * (k % 2);
    this.dm.set(word, u32((this.word(word) & ~(0xffff << sh)) | ((v & 0xffff) << sh)));
  }

  setType(track: number, t: number): void {
    const word = nw(TYPE_CACHE) + Math.floor(track / 2);
    const sh = 16 * (track % 2);
    this.dm.set(word, u32((this.word(word) & ~(0xffff << sh)) | (t << sh)));
  }

  setNote(track: number, note: number, tune = 64, vel = 100): void {
    this.hw(1 + track, Math.trunc(note * 256));
    this.hw(0x6d + 0x30 * track, Math.trunc(tune * 256));
    this.hw(0x1a + track, vel << 8);
  }

  recWord(v: number, off: number, value: number): void {
    this.dm.set(nw(RECORDS + v * REC + off), u32(value));
  }

  /** The DM as ops: every word of `dm`, then the call. */
  private async run(
    entry: number,
    regs: Record<string, number>,
    reads: Record<string, [number, number]>,
    maxSteps: number,
    breaks: number[],
    readRegs: string[],
  ): Promise<{ res: CallResult; names: string[] }> {
    const names = Object.keys(reads);
    const { port, first } = await this.open({
      return_sw: RETURN_SW,
      breaks,
      code: [this.b.core, this.b.routine].map((l) => ({ address: SW_ALIAS_BASE + 2 * l.baseSw, hex: l.codeBoot })),
      ops: [
        { poke: [...this.dm].map(([a, w]) => [a, u32(w)] as [number, number]) },
        {
          call: {
            pc: entry,
            regs: Object.fromEntries(Object.entries(regs).map(([n, v]) => [n, u32(v)])),
            max_steps: maxSteps,
            reads: names.map((n) => reads[n]!),
            read_regs: readRegs,
          },
        },
      ],
    });
    await port.close();
    return { res: first.at(-1)!, names };
  }

  /**
   * One call the way the engine makes it. `ctx` is the context registers: undefined draws a new hostile context from the rig's seed,
   * "friendly" the constants the old rig assumed, an object is taken as given (missing names friendly).
   */
  async call(v: number, o: CallOptions = {}): Promise<VoiceCall> {
    const ctx: Context =
      o.ctx === undefined
        ? drawContext(this.ctxRng)
        : o.ctx === 'friendly'
          ? { ...FRIENDLY }
          : { ...FRIENDLY, ...o.ctx };
    this.lastCtx = { ...ctx };
    this.dm.set(nw(MASK), o.mask === undefined ? this.tableMask() : o.mask);
    const saved: Record<string, number> = {};
    for (const n of [3, 5, 6, 7, 9, 10, 11, 13, 15]) saved[`R${n}`] = 0x7000 + n;
    Object.assign(saved, { I0: 0x20000010, I1: 0x20000020, I2: 0x20000030, I3: 0x20000040, I5: 0x20000050 });
    Object.assign(saved, { M1: 11, M2: 12, M3: 13, I8: 0x20000080, I9: 0x20000090, M8: 5 });
    const regs: Record<string, number> = {
      R4: RECORDS + v * REC,
      R8: OUT + v * 0x80,
      R12: 32,
      R14: 32 - v,
      ...ctx,
      I6: STACK,
      I7: STACK,
      ...saved,
      ...o.regs,
    };
    this.dm.set(nw(STACK) - 1, RETURN_SW - 1); // the return address the CJUMP pushed: [I6 - 1]
    this.dm.set(nw(STACK), 0x20000ff0); // the caller's frame pointer: [I6]
    const reads: Record<string, [number, number]> = {
      out: [nw(OUT + v * 0x80), 32],
      table: [nw(TABLE), 4],
      ...o.reads,
    };
    return this.finish(
      await this.run(
        this.coreSymbols._dv_entry!,
        regs,
        reads,
        o.maxSteps ?? 20000,
        [STOCK_BP, ...(o.breaks ?? [])],
        READ_REGS,
      ),
      regs,
      READ_REGS,
    );
  }

  /** The frame consumer's call of the per-track unpack (R12 = track, R8 = its even voice), hooked by `_dv_unpack`; `reads.lev` is knob 7's frame word after it. */
  async unpack(track: number, mask?: number): Promise<VoiceCall> {
    this.dm.set(nw(MASK), mask === undefined ? this.tableMask() : mask);
    const regs = { ...FRIENDLY, R12: track, R8: 2 * track, I6: STACK, I7: STACK };
    const reads: Record<string, [number, number]> = { lev: [nw(FRAME + 0xec + 0x60 * track), 1] };
    return this.finish(await this.run(this.coreSymbols._dv_unpack!, regs, reads, 20000, [UNPACK_BP], []), regs, []);
  }

  private finish(
    { res, names }: { res: CallResult; names: string[] },
    given: Record<string, number>,
    readRegs: readonly string[],
  ): VoiceCall {
    for (const n of readRegs) {
      if (res.regs[n] == null) throw new Error(`the rig did not report register ${n} (call at pc ${res.final_pc})`);
    }
    return {
      halt: res.halt,
      finalPc: res.final_pc,
      instructions: res.instructions,
      regs: Object.fromEntries(Object.entries(res.regs).map(([n, v]) => [n, v as number])),
      in: given,
      reads: Object.fromEntries(names.map((n, i) => [n, res.reads[i] ?? []])),
    };
  }
}

/** The call stopped at the stand-in SW (a return or a break) and not by a step limit or a trap. */
export const stopsAt = (out: VoiceCall, sw: number): boolean => out.halt === 'breakpoint' && out.finalPc === sw;

/** A routine that stores the three breadcrumb words and the two ring words it sees and clears the block (`CrumbRig` of test_dsp_voice.py). */
export const CRUMB_PROBE = `
.SECTION/PM seg_pmco;
.GLOBAL _crumb_probe;
_crumb_probe:
    I0 = 0x2009B000;
    R0 = DM(0x2009D030);
    DM(0, I0) = R0;
    R0 = DM(0x200A0000 + 0x300);
    DM(1, I0) = R0;
    R0 = DM(0x200A2000 + 0x300);
    DM(2, I0) = R0;
    R0 = DM(0x2009D034);
    DM(3, I0) = R0;
    I0 = R8;
    R0 = 0;
    LCNTR = 32, DO _cp_z UNTIL LCE;
_cp_z: DM(I0, M6) = R0;
    JUMP (M13, I13);
`;

/** The wrapper around `CRUMB_PROBE`. */
export function crumbRig(defines: readonly string[] = [], seed?: number): VoiceRig {
  return new VoiceRig({
    defines,
    ...(seed === undefined ? {} : { seed }),
    routine: { text: CRUMB_PROBE, base: 0xb93c00, externs: () => ({}), symbol: '_crumb_probe' },
  });
}
