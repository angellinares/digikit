/**
 * A rig for the FX adapter core/fx: its chain on the generated native core, one block per call, as the hook makes it (port of
 * tests/support/fx_rig.py). The send route has its own entry (`sendBlock`: the send hook before the master function) and its buses:
 * `setTrack` (DT2 track bus), `setSlot`, `setSlotmap` and `setFade` (DN2 slot bus, map and fade flags), `setReturn`, `setAmounts`.
 *
 *     const rig = await FxRig.open([{ pc: '_te_half', state: '_te_state_a' }], { sources });
 *     const b = await rig.block(master);   // master: 64 floats (32 left, 32 right) before the hook
 *     b.master                             // the 64 floats after the hook; b.instructions, b.returned, b.ctxBad, b.regsBad
 *
 * The adapter (modules/core/adapters/fx/src/dsp/fx.s) and the effect sources are assembled and linked the way the patch does: the
 * table at TABLE, the master block at MASTER, the hook's tail (the stock function's second instruction) at STOCK_BP. `.PKBSS` memory
 * (the effects' states) starts poisoned, so an effect that reads a state the adapter did not zero shows. Every call draws a hostile
 * context (M5 ... MODE1) and fills the registers the stock function expects back with markers; both must come back unchanged.
 */
import {
  CONTEXT_REGS,
  RETURN_SW,
  STACK,
  STOCK_BP,
  TABLE,
  contextBad,
  f2w,
  nw,
  resolveContext,
  w2f,
  type Context,
} from './context.ts';
import { OpQueue, hex, realRig, type RigOpener } from './ops.ts';
import {
  CODE_BASE,
  FX_FADETAB,
  FX_MASTER,
  FX_RETURN,
  FX_SLOTMAP,
  FX_TRACKBUS,
  buildFx,
  type FxBuilt,
} from './place.ts';
import { PyRandom } from './prng.ts';
import type { CallResult, RigPort } from './rig.ts';
import { SW_ALIAS_BASE } from './toolchain.ts';

export const ROW_WORDS = 24;
const FREE_PC = 0xffffffff; // the pc of a free row (core/dynfx): the adapter skips it
const POISON = 0x7fc00001; // the initial word of every .PKBSS
export const TRACKS = 16;
export const MAX_ROWS = 4;
// the row words (core/fx module.toml)
export const PC = 0;
export const DESC = 1;
export const STATE = 2;
export const ENABLED = 3;
export const P0 = 4;
export const WAS = 12;
export const MIX = 13;
export const DIS = 14;
export const NFAULT = 17;
export const ROUTE = 18;
export const AMT0 = 19;
const ROUTE_MASTER = 0;
export const ROUTE_SEND = 1;
const ONE = 0x3f800000; // 1.0f: the default mix of a row

/** Registers the stock function expects back (callee-saved, its arguments) and the markers they carry into the hook. */
const MARKS: Record<string, number> = {};
for (let n = 0; n < 16; n++) MARKS[`R${n}`] = 0x7000 + n;
Object.assign(MARKS, {
  I0: 0x20000010,
  I1: 0x20000020,
  I2: 0x20000030,
  I3: 0x20000040,
  I4: 0x20000050,
  I5: 0x20000060,
  I12: 0x20000070,
  I13: 0x20000080,
  I8: 0x20000090,
  I9: 0x200000a0,
  I10: 0x200000b0,
  I11: 0x200000c0,
  I14: 0x200000d0,
  I15: 0x200000e0,
  M1: 11,
  M2: 12,
  M3: 13,
  M4: 14,
  M8: 5,
  M9: 6,
  M10: 7,
  M11: 8,
  M12: 9,
  B3: 0x20000040,
});

export interface Row {
  /** the process routine's symbol; "" is a free row (pc 0xffffffff) */
  pc: string;
  /** the state block's data symbol ("": the rig reserves `_fx_auto_state<row>`, as the runtime does for a run-time effect) */
  state: string;
  enabled?: number;
  params?: number[];
  desc?: string;
  /** row word 13: the dry/wet blend (a float, 0 = dry, 1.0 = wet) */
  mix?: number;
  /** row word 18: 0 master chain, 1 send bus */
  route?: number;
  /** row words 19 to 22: the send amounts of the 16 tracks, bytes 0 to 127 (fewer than 16: the rest 0) */
  amounts?: number[];
}

/** The 4 words that hold 16 amount bytes, track 4w + b in byte b of word w. */
export function amountWords(amounts: readonly number[]): number[] {
  const a = [...amounts, ...Array<number>(TRACKS).fill(0)].slice(0, TRACKS);
  return [0, 1, 2, 3].map((w) => [0, 1, 2, 3].reduce((sum, b) => sum + (a[4 * w + b]! & 0xff) * 2 ** (8 * b), 0));
}

export interface FxBlock {
  /** the 64 floats after the hook */
  master: number[];
  /** the 64 floats of the FX return bus after the hook */
  ret: number[];
  instructions: number;
  /** the hook ended in the stock function's second instruction */
  returned: boolean;
  halt: string;
  finalPc: number;
  /** context registers that did not come back */
  ctxBad: string[];
  /** marked registers that did not come back */
  regsBad: string[];
  /** the fault count word (row word 17) of each row after the hook */
  faults: number[];
  /** the effect's output block as it left it (64 floats; empty when the adapter has no scratch symbol) */
  scratch: number[];
  /** the stores of the module's code in this call (with `recordStores`) */
  stores: CallResult['stores'];
  /** the DM words asked for with `extraReads`, one row each (a word that nothing wrote is null) */
  extra: (number | null)[][];
}

export interface FxRigOptions {
  sources: readonly string[];
  seed?: number;
  stepLimit?: number;
  /** record the stores of the module's code (true) or of all code (`all`) */
  recordStores?: boolean | 'all';
  device?: 'dt2' | 'dn2';
  open?: RigOpener;
  /** the placed and linked code: built from `sources` by default (a host test passes a made-up one) */
  built?: FxBuilt;
}

export class FxRig {
  readonly built: FxBuilt;
  readonly stepLimit: number;
  lastCtx: Context = {};
  private readonly rows: Row[];
  private readonly ctxRng: PyRandom;
  private readonly q = new OpQueue();
  private port!: RigPort;
  private readonly recordStores: boolean | 'all';

  private constructor(rows: Row[], o: FxRigOptions) {
    if (rows.length > MAX_ROWS) throw new Error(`a chain has at most ${MAX_ROWS} effects`);
    this.built = o.built ?? buildFx(o.sources, o.device ?? 'dt2');
    this.rows = rows;
    this.ctxRng = new PyRandom(o.seed ?? 0xd1617);
    this.stepLimit = o.stepLimit ?? 200_000;
    this.recordStores = o.recordStores ?? false;
  }

  static async open(rows: Row[], o: FxRigOptions): Promise<FxRig> {
    const rig = new FxRig(rows, o);
    await rig.reset(o.open ?? realRig);
    return rig;
  }

  async close(): Promise<void> {
    await this.port.close();
  }

  private async reset(open: RigOpener): Promise<void> {
    const b = this.built;
    const q = new OpQueue();
    for (let k = 0; k < (MAX_ROWS + 1) * ROW_WORDS; k++) q.poke(nw(TABLE) + k, 0);
    for (let k = 0; k < 64; k++) {
      q.poke(nw(FX_MASTER) + k, 0);
      q.poke(nw(FX_RETURN) + k, 0);
    }
    for (let k = 0; k < TRACKS * 64; k++) q.poke(nw(FX_TRACKBUS) + k, 0);
    for (let k = 0; k < 8; k++) q.poke(nw(FX_SLOTMAP) + k, 0);
    for (let k = 0; k < 4; k++) q.poke(nw(FX_FADETAB) + k, 0);
    for (const init of b.inits) {
      const view = new DataView(init.bytes.buffer, init.bytes.byteOffset, init.bytes.byteLength);
      for (let k = 0; k < Math.floor(init.bytes.length / 4); k++)
        q.poke(nw(init.address) + k, init.bss ? POISON : view.getUint32(4 * k, true));
    }
    this.rows.forEach((r, i) => {
      const base = nw(TABLE) + i * ROW_WORDS;
      if (!r.pc) {
        q.poke(base + PC, FREE_PC);
        return;
      }
      q.poke(base + PC, b.symbols[r.pc]!);
      q.poke(base + DESC, b.data[r.desc ?? '_te_desc']!);
      q.poke(base + STATE, b.data[this.stateSymbol(i)]!);
      q.poke(base + ENABLED, r.enabled ?? 1);
      q.poke(base + MIX, r.mix ?? ONE);
      q.poke(base + ROUTE, r.route ?? ROUTE_MASTER);
      amountWords(r.amounts ?? []).forEach((word, w) => q.poke(base + AMT0 + w, word));
      (r.params ?? [0, 0, 0, 0]).forEach((p, j) => q.poke(base + P0 + j, p));
    });
    const { port } = await open({
      return_sw: RETURN_SW,
      breaks: [STOCK_BP],
      ...(this.recordStores ? { watch: this.recordStores === 'all' ? [[CODE_BASE, STOCK_BP]] : b.moduleCode } : {}),
      code: b.linked.map((l) => ({ address: SW_ALIAS_BASE + 2 * l.baseSw, hex: l.codeBoot })),
      ops: q.take(),
    });
    this.port = port;
  }

  /** The byte address of the state block of row i. */
  stateAddress(i: number): number {
    return this.built.data[this.stateSymbol(i)]!;
  }

  /** Set the first COUNT words of the state block of row I to WORD (before the first block): an effect must not depend on what the state held. */
  fillState(i: number, word: number, count: number): void {
    const base = nw(this.stateAddress(i));
    for (let k = 0; k < count; k++) this.q.poke(base + k, word >>> 0);
  }

  private stateSymbol(i: number): string {
    return this.rows[i]!.state || `_fx_auto_state${i}`;
  }

  setRowWord(i: number, word: number, value: number): void {
    this.q.poke(nw(TABLE) + i * ROW_WORDS + word, value);
  }

  setEnabled(i: number, on: boolean): void {
    this.setRowWord(i, ENABLED, on ? 1 : 0);
  }

  /** The 64 floats (32 left, 32 right) of track bus T, as the sends driver leaves them before the master function. */
  setTrack(t: number, samples: readonly number[]): void {
    samples.forEach((x, k) => this.q.poke(nw(FX_TRACKBUS + 0x100 * t) + k, f2w(x)));
  }

  /** Digitone II: the 64 floats of voice slot V's bus (the same cells as the Digitakt II's track buses). */
  setSlot(v: number, samples: readonly number[]): void {
    this.setTrack(v, samples);
  }

  /** Digitone II: the slot-to-track map, the track of each of the 16 slots (slot v in half v % 2 of word v / 2, low half first). */
  setSlotmap(tracks: readonly number[]): void {
    const t = [...tracks, ...Array<number>(TRACKS).fill(0)].slice(0, TRACKS);
    for (let k = 0; k < 8; k++)
      this.q.poke(nw(FX_SLOTMAP) + k, (t[2 * k]! & 0xffff) | ((t[2 * k + 1]! & 0xffff) << 16));
  }

  /** Digitone II: the slot fade flags: the slots in SLOTS get the flag byte 1, the others 0. */
  setFade(slots: Iterable<number>): void {
    const on = new Set(slots);
    const flags: number[] = Array.from({ length: TRACKS }, (_, v) => (on.has(v) ? 1 : 0));
    for (let k = 0; k < 4; k++)
      this.q.poke(
        nw(FX_FADETAB) + k,
        flags.slice(4 * k, 4 * k + 4).reduce((sum, f, j) => sum + f * 2 ** (8 * j), 0),
      );
  }

  /** The FX return bus as the stock sends driver wrote it. */
  setReturn(samples: readonly number[]): void {
    samples.forEach((x, k) => this.q.poke(nw(FX_RETURN) + k, f2w(x)));
  }

  /** Row I: the route word and the 16 amount bytes (words 19 to 22). */
  setAmounts(i: number, amounts: readonly number[], route = ROUTE_SEND): void {
    this.setRowWord(i, ROUTE, route);
    amountWords(amounts).forEach((w, k) => this.setRowWord(i, AMT0 + k, w));
  }

  /** The DM words at byte address ADDRESS now (null: nothing wrote the word). Pending pokes are sent first. */
  async peek(address: number, count = 1): Promise<(number | null)[]> {
    this.q.push({ call: { pc: RETURN_SW, regs: {}, max_steps: 0, reads: [[nw(address), count]], read_regs: [] } });
    const res = (await this.port.run(this.q.take())).at(-1)!;
    return res.reads[0] ?? [];
  }

  private async word(address: number): Promise<number> {
    return (await this.peek(address))[0] ?? 0;
  }

  /** Row word WORD of row I. */
  rowWord(i: number, word: number): Promise<number> {
    return this.word(TABLE + 4 * (i * ROW_WORDS + word));
  }

  /** Word K of the state block of row I. */
  stateWord(i: number, k = 0): Promise<number> {
    return this.word(this.stateAddress(i) + 4 * k);
  }

  /** Word K of a data symbol. */
  symbolWord(name: string, k = 0): Promise<number> {
    return this.word(this.built.data[name]! + 4 * k);
  }

  /** The send bus as the last send block built it (32 left, 32 right). */
  async bus(): Promise<number[]> {
    const words = await this.peek(this.built.data._fxs_bus! + 4, 64);
    return words.map((w) => w2f(w ?? 0));
  }

  /** The 16 gains the send pass of row I ended its last block with. */
  async prevGains(i: number): Promise<number[]> {
    const words = await this.peek(this.built.data._fxs_prev! + 64 * i, 16);
    return words.map((w) => w2f(w ?? 0));
  }

  /** One block of the send hook (the master function's entry): the tracks, the return and the master as set before the call. */
  sendBlock(
    master: readonly number[] = Array<number>(64).fill(0),
    ctx?: Context | 'friendly',
    regs: Record<string, number> = {},
  ): Promise<FxBlock> {
    return this.block(master, ctx, regs, [], '_fxs_entry');
  }

  /** The words of a `dk_fx_desc` data symbol as built (before any call). */
  descWords(symbol: string, n = 6): number[] {
    const at = this.built.data[symbol];
    for (const init of this.built.inits)
      if (at !== undefined && init.address <= at && at < init.address + init.bytes.length) {
        const view = new DataView(init.bytes.buffer, init.bytes.byteOffset, init.bytes.byteLength);
        return Array.from({ length: n }, (_, k) => view.getUint32(at - init.address + 4 * k, true));
      }
    throw new Error(`no data symbol ${symbol}`);
  }

  /** One block of the hook: the master as set before the call, and the registers and context the stock function holds. */
  async block(
    master: readonly number[],
    ctx?: Context | 'friendly',
    regs: Record<string, number> = {},
    extraReads: [address: number, count: number][] = [],
    entry = '_fx_entry',
  ): Promise<FxBlock> {
    const context = resolveContext(ctx, this.ctxRng);
    this.lastCtx = { ...context };
    const q = this.q;
    master.forEach((x, k) => q.poke(nw(FX_MASTER) + k, f2w(x)));
    q.poke(nw(STACK) - 1, RETURN_SW - 1);
    q.poke(nw(STACK), 0x20000ff0);
    const marks = { ...MARKS, ...regs };
    const names = Object.keys(marks).sort();
    const reads: [number, number][] = [[nw(FX_MASTER), 64]];
    for (let i = 0; i < MAX_ROWS; i++) reads.push([nw(TABLE) + i * ROW_WORDS + NFAULT, 1]);
    const scratch = this.built.data._fx_scratch;
    if (scratch !== undefined) reads.push([nw(scratch), 64]);
    const firstExtra = reads.length;
    for (const [a, n] of extraReads) reads.push([nw(a), n]);
    reads.push([nw(FX_RETURN), 64]);
    q.push({
      call: {
        pc: this.built.symbols[entry]!,
        regs: { ...marks, ...context, I6: STACK, I7: STACK },
        max_steps: this.stepLimit,
        reads,
        read_regs: ['I6', 'I7', ...names, ...CONTEXT_REGS],
      },
    });
    const [res] = (await this.port.run(q.take())) as [CallResult];
    const returned = res.halt === 'breakpoint' && res.final_pc === STOCK_BP;
    const reported = (n: string): number => {
      const v = res.regs[n];
      if (v == null) throw new Error(`the rig did not report register ${n} (call at pc ${res.final_pc})`);
      return v >>> 0;
    };
    const regsBad: string[] = [];
    if (returned) {
      regsBad.push(...names.filter((n) => reported(n) !== marks[n]! >>> 0));
      for (const n of ['I6', 'I7']) if (reported(n) !== STACK) regsBad.push(n);
    }
    return {
      master: Array.from({ length: 64 }, (_, k) => w2f(res.reads[0]?.[k] ?? 0)),
      ret: Array.from({ length: 64 }, (_, k) => w2f(res.reads[firstExtra + extraReads.length]?.[k] ?? 0)),
      instructions: res.instructions,
      returned,
      halt: res.halt,
      finalPc: res.final_pc,
      ctxBad: returned ? contextBad(res.regs, context, res.final_pc) : [],
      regsBad,
      faults: Array.from({ length: MAX_ROWS }, (_, i) => res.reads[1 + i]?.[0] ?? 0),
      scratch:
        scratch === undefined ? [] : Array.from({ length: 64 }, (_, k) => w2f(res.reads[1 + MAX_ROWS]?.[k] ?? 0)),
      stores: res.stores,
      extra: extraReads.map((_, i) => res.reads[firstExtra + i] ?? []),
    };
  }

  /**
   * The DM that the module's code may store to in a call (docs/design/module-sdk.md 11.5 D6, <dk/fx/fx.h>): the output block, its own data
   * and bss (its state included) and `stackBytes` of data stack below I7 (the adapter does not lower I7 before the call).
   */
  private allowed(stackBytes: number): { address: number; bytes: number }[] {
    return [
      { address: this.built.data._fx_scratch!, bytes: 4 * 64 },
      ...this.built.moduleData,
      { address: STACK - stackBytes, bytes: stackBytes },
    ];
  }

  /** Stores of a call made by the module's code outside `allowed`, as text (at most 4). Needs `recordStores`. */
  foreignStores(stackBytes: number, stores: CallResult['stores']): string[] {
    const allowed = this.allowed(stackBytes);
    const names = [
      { address: this.built.data._fx_save!, bytes: 128, name: 'adapter save area' },
      { address: this.built.data._fx_in!, bytes: 16, name: 'dk_fx_in' },
      { address: FX_MASTER, bytes: 256, name: 'the master block (read only for an effect)' },
      { address: TABLE, bytes: (MAX_ROWS + 1) * ROW_WORDS * 4, name: 'the chain table' },
    ];
    const found: string[] = [];
    for (const [pc, address, width] of stores) {
      if (!this.built.moduleCode.some(([lo, hi]) => lo <= pc && pc < hi)) continue;
      const at = address >= SW_ALIAS_BASE ? address - SW_ALIAS_BASE : address;
      if (allowed.some((a) => a.address <= at && at + width <= a.address + a.bytes)) continue;
      const near = names.find((n) => n.address <= at && at < n.address + n.bytes);
      found.push(
        `store of ${width} bytes to ${hex(at)} at pc ${hex(pc)}, ${near ? `inside ${near.name} +${hex(at - near.address)}` : 'unknown memory'}`,
      );
      if (found.length === 4) break;
    }
    return found;
  }
}
