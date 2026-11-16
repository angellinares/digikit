/**
 * A rig for any machine module: its DSP routine on the generated native core, through core/dsp-voice, block by block as the engine
 * calls it (port of tests/support/machine_rig.py).
 *
 *     const rig = await MachineRig.open(spec);
 *     rig.setParams({ decay: 0x7f00 });
 *     rig.setNote(60, 100); rig.trigger();
 *     const b = await rig.block();      // b.samples (32 floats), b.instructions, b.returned, b.contextBad
 *     await rig.close();
 *
 * Per device (the engine's contract, core/dsp-voice headers):
 *   dt2  voice record per voice (the even voice 2t of track t), trigger = record word +0x1b8 set to 1 (ACTIVE, mark cleared); the call
 *        returns to the caller (RETURN_SW). Frame: pitch hw 1 + t, velocity hw 0x1a + t, machine type in the type cache.
 *   dn2  voice slot v, trigger = the rising edge of bit v of frame hw 0x11 (one block), pitch hw 1 + v, velocity hw 26 + v, type hw
 *        74 + v; the call ends in the stock helper (STOCK_BP).
 * Unwritten `.PKBSS` memory reads as NaN and the output block is NaN before every call, so a routine that reads what it did not
 * write, or does not write the whole block, shows in the samples. The context registers (M5 ... MODE1) are drawn hostile per call
 * from the rig's seed and must come back unchanged.
 */
import {
  CONTEXT_REGS,
  DIAG,
  FRAME,
  OUT,
  RECORDS,
  REC,
  RELEASE,
  RETURN_SW,
  STACK,
  STOCK_BP,
  TABLE,
  TYPE_CACHE,
  UNPACK_BP,
  contextBad,
  nw,
  resolveContext,
  type Context,
  w2f,
} from './context.ts';
import { OpQueue, hex, pokeHalf, realRig, type RigOpener } from './ops.ts';
import { buildMachine, type MachineBuilt, type Range } from './place.ts';
import { PyRandom } from './prng.ts';
import type { CallResult, RigPort } from './rig.ts';
import { frameHw, galleryGaps, machineLabel, type MachineSpec } from './spec.ts';
import { SW_ALIAS_BASE } from './toolchain.ts';

const NAN = 0xffffffff;
const OUT_DN2 = OUT + 0x1000; // DN2 voice buffers (low 7 bits of the address are 0x10: the wrapper checks)
const ROW_WORDS = 24; // words of a registry row of core/dsp-voice
/** The module stack grows below I7 at entry, exclusive. DT2 lowers I7 by 40 words before entering the module; DN2 saves its context in static memory and does not. */
const ADAPTER_FRAME_WORDS: Record<string, number> = { dt2: 40, dn2: 0 };
const DEFAULT_STACK_BYTES = 256; // a raw routine (no dk_machine_desc) declares no stack

export interface MachineBlock {
  /** 32 float32 values (NaN where the routine did not write) */
  samples: number[];
  instructions: number;
  /** the call came back to the caller within the step limit */
  returned: boolean;
  halt: string;
  finalPc: number;
  /** context registers that did not come back as they went in */
  contextBad: string[];
  /** the runtime guard's fault count of the type since the start (registry row word 13) */
  faults: number;
  /** stores of the module's code outside the memory it may write (with `recordStores`), one text each */
  foreign: string[];
  /** DT2: the odd voice's block after the call (a stereo machine's R; NaN when nothing wrote it) */
  right: number[];
  /** DT2: the voice record's word +0x1b8 after the call: byte 0 is ACTIVE, byte 1 the note-on mark (0 on DN2, which has no record) */
  active: number;
  /** the DM words asked for with `extraReads`, one row each (a word that nothing wrote is null) */
  extra: (number | null)[][];
  /** instructions executed per pc (short-word address) in this call; empty without the `profile` option */
  profile: [pc: number, count: number][];
}

export interface MachineRigOptions {
  seed?: number;
  track?: number;
  stepLimit?: number;
  recordStores?: boolean;
  /** ask the rig for the instructions per pc of every `block` call (`MachineBlock.profile`) */
  profile?: boolean;
  /** `.PKBSS` memory (the module's bss and the voice state blocks) starts as NaN words (default true); false zeroes it and needs a reason at the call site (sound-check cases: `poison_reason`) */
  poison?: boolean;
  /** with `poison`: fill the memory with a seeded integer pattern instead of NaN words (a second fill, to show that the output does not depend on the initial state) */
  fillSeed?: number;
  open?: RigOpener;
  /** the placed and linked code: built from the spec by default (a host test passes a made-up one) */
  built?: MachineBuilt;
}

const words = (row: (number | null)[] | undefined, n: number): number[] =>
  Array.from({ length: n }, (_, k) => w2f(row?.[k] ?? NAN));

export class MachineRig {
  readonly spec: MachineSpec;
  readonly built: MachineBuilt;
  readonly device: string;
  readonly stepLimit: number;
  track: number;
  voice = 0;
  lastCtx: Context = {};
  private readonly ctxRng: PyRandom;
  private readonly byId: Map<string, MachineSpec['params'][number]>;
  private readonly q = new OpQueue();
  private trig = false;
  private release_ = false;
  private port!: RigPort;
  private readonly recordStores: boolean;
  private readonly profile: boolean;
  private readonly poison: boolean;
  private readonly fillSeed: number | undefined;

  private constructor(spec: MachineSpec, o: MachineRigOptions) {
    this.spec = spec;
    this.built = o.built ?? buildMachine(spec);
    this.device = spec.device;
    this.track = o.track ?? 0;
    this.stepLimit = o.stepLimit ?? 400_000;
    this.recordStores = o.recordStores ?? false;
    this.profile = o.profile ?? false;
    this.poison = o.poison ?? true;
    this.fillSeed = o.fillSeed;
    this.ctxRng = new PyRandom(o.seed ?? 0xd1617);
    this.byId = new Map(spec.params.map((p) => [p.id, p]));
  }

  /** A fresh core: zeroed stand-ins, initialised data, parameters at their defaults, the voice idle. */
  static async open(spec: MachineSpec, o: MachineRigOptions = {}): Promise<MachineRig> {
    if (!o.built) {
      const gaps = galleryGaps(spec.module.split('#')[0]!).filter((g) => g.startsWith(`${spec.device}:`));
      if (gaps.length)
        throw new Error(`${spec.module}: ${gaps.join('; ')}; run \`mise run module:check ${spec.module}\` to write it`);
    }
    const rig = new MachineRig(spec, o);
    await rig.reset(o.open ?? realRig);
    return rig;
  }

  async close(): Promise<void> {
    await this.port.close();
  }

  /** The initial word of a poisoned memory word: NaN, or a seeded integer pattern (xorshift of the seed and the word address). */
  private fillWord(address: number): number {
    if (this.fillSeed === undefined) return NAN;
    let x = (Math.imul(address + 1, 0x9e3779b1) ^ this.fillSeed) >>> 0 || 1;
    x ^= x << 13;
    x >>>= 0;
    x ^= x >>> 17;
    x ^= x << 5;
    return x >>> 0;
  }

  private async reset(open: RigOpener): Promise<void> {
    const { built: b, spec } = this;
    const q = new OpQueue();
    for (let k = 0; k < 1024; k++) q.poke(nw(FRAME) + k, 0);
    for (let k = 0; k < 8; k++) {
      q.poke(nw(TYPE_CACHE) + k, 0);
      q.poke(nw(RELEASE) + k, 0);
    }
    for (let k = 0; k < Math.floor((32 * REC) / 4); k++) q.poke(nw(RECORDS) + k, 0);
    for (let k = 0; k < 2 * ROW_WORDS; k++) q.poke(nw(TABLE) + k, 0);
    q.poke(nw(TABLE), spec.machineType);
    q.poke(nw(TABLE) + 1, b.symbols[spec.entry]!);
    for (const p of spec.params)
      // words 2 to 9: the declared range of knob j (min | max << 16; none for a stock knob)
      if (!p.stock && 0 <= p.lo && p.lo <= p.hi && p.hi <= 0xffff) q.poke(nw(TABLE) + 2 + p.knob, p.lo | (p.hi << 16));
    if (spec.noteOn !== null) {
      // words 16 to 19: note_on pc, state base, stride, clear bytes; word 22: the latched words
      q.poke(nw(TABLE) + 16, b.symbols[spec.noteOn]!);
      q.poke(nw(TABLE) + 17, b.state);
      q.poke(nw(TABLE) + 18, b.stride);
      q.poke(nw(TABLE) + 19, b.clear);
      q.poke(
        nw(TABLE) + 22,
        spec.params.reduce((m, p) => (p.latchedWord !== null ? m | (1 << p.latchedWord) : m), 0),
      );
    }
    for (const init of b.inits) {
      const view = new DataView(init.bytes.buffer, init.bytes.byteOffset, init.bytes.byteLength);
      for (let k = 0; k < Math.floor(init.bytes.length / 4); k++)
        q.poke(
          nw(init.address) + k,
          init.bss && this.poison ? this.fillWord(nw(init.address) + k) : view.getUint32(4 * k, true),
        );
    }
    const { port } = await open({
      return_sw: RETURN_SW,
      breaks: [STOCK_BP, UNPACK_BP],
      ...(this.recordStores ? { watch: b.moduleCode } : {}),
      code: b.linked.map((l) => ({ address: SW_ALIAS_BASE + 2 * l.baseSw, hex: l.codeBoot })),
      ops: q.take(),
    });
    this.port = port;
    this.setVoice(this.track);
    const t = this.track;
    if (this.device === 'dt2') this.setTypeDt2(t, spec.machineType);
    else {
      this.hw(74 + this.voice, spec.machineType);
      this.setOwner(t);
    }
    for (const p of spec.params) this.setParam(p.id, p.default);
    this.setNote(60, 100);
    this.trig = this.release_ = false;
  }

  private setVoice(track: number): void {
    this.track = track;
    this.voice = this.device === 'dn2' ? track : 2 * track;
  }

  /** Make `track` the voice of the following calls (state and memory stay: several voices run on one core). */
  use(track: number): void {
    this.setVoice(track);
    this.trig = this.release_ = false;
  }

  /** `use` for a voice that has not run yet: it gets the machine type, its parameters at their defaults and a note, like a fresh rig's voice. */
  select(track: number): void {
    this.use(track);
    if (this.device === 'dt2') this.setTypeDt2(track, this.spec.machineType);
    else {
      this.hw(74 + this.voice, this.spec.machineType);
      this.setOwner(track);
    }
    for (const p of this.spec.params) this.setParam(p.id, p.default);
    this.setNote(60, 100);
  }

  /** Write a DM word (a fixture's memory: the sample pool). */
  poke(address: number, word: number): void {
    this.q.poke(address, word);
  }

  hw(k: number, value: number): void {
    pokeHalf(this.q, nw(FRAME), k, value);
  }

  private setTypeDt2(track: number, t: number): void {
    const shift = 16 * (track % 2);
    this.q.pokeMasked(nw(TYPE_CACHE) + Math.floor(track / 2), 0xffff << shift, (t & 0xffff) << shift);
  }

  /** Set DN2's current frame owner independently of the physical voice slot. */
  setOwner(track: number): void {
    if (this.device !== 'dn2') throw new Error('only DN2 slots have a separate frame owner');
    this.hw(0x52d + this.voice, track);
  }

  setParam(id: string, word: number): void {
    const p = this.byId.get(id);
    if (!p) throw new Error(`${machineLabel(this.spec)}: no parameter ${id}`);
    this.hw(frameHw(this.spec.frame, this.device === 'dt2' ? this.track : this.voice, p.knob), word);
  }

  setParams(values: Iterable<readonly [string, number]>): void {
    for (const [id, w] of values) this.setParam(id, w);
  }

  setNote(note: number, vel = 100): void {
    const v = this.device === 'dt2' ? this.track : this.voice;
    this.hw(1 + v, Math.trunc(note * 256));
    this.hw(0x1a + v, vel << 8);
  }

  /** A new note at the next block (DT2: the arm routine's store; DN2: the trig bit, one block). */
  trigger(): void {
    this.trig = true;
  }

  /** A note-off at the next block: the voice's release byte is 1 for that block (the firmware's pulse). */
  release(): void {
    this.release_ = true;
  }

  private releaseByte(value: number): void {
    const shift = 8 * (this.voice % 4);
    this.q.pokeMasked(nw(RELEASE) + Math.floor(this.voice / 4), 0xff << shift, value << shift);
  }

  private trigBit(on: boolean): void {
    const bit = (1 << this.voice) << 16; // bit v of frame halfword 0x11 (the upper half of word 8)
    this.q.pokeMasked(nw(FRAME) + 8, bit, on ? bit : 0);
  }

  private regs(ctx: Context): { regs: Record<string, number>; out: number } {
    const saved: Record<string, number> = {};
    for (const n of [3, 5, 6, 7, 9, 10, 11, 13, 15]) saved[`R${n}`] = 0x7000 + n;
    Object.assign(saved, { I0: 0x20000010, I1: 0x20000020, I2: 0x20000030, I3: 0x20000040, I5: 0x20000050 });
    Object.assign(saved, { M1: 11, M2: 12, M3: 13, I8: 0x20000080, I9: 0x20000090, M8: 5 });
    const v = this.voice;
    let regs: Record<string, number>;
    let out: number;
    if (this.device === 'dt2') {
      out = OUT + v * 0x80;
      regs = { R4: RECORDS + v * REC, R8: out, R12: 32, R14: 32 - v };
    } else {
      out = OUT_DN2 + (v + 1) * 0x100 + 0x10;
      regs = { R8: out, R12: out, R4: 0, R15: 16 - v };
    }
    return { regs: { ...saved, ...regs, ...ctx, I6: STACK, I7: STACK }, out };
  }

  /** One call the way the engine makes it. `ctx` as in drawContext (undefined draws a hostile context, "friendly" the constants). */
  async block(ctx?: Context | 'friendly', extraReads: [address: number, count: number][] = []): Promise<MachineBlock> {
    const context = resolveContext(ctx, this.ctxRng);
    this.lastCtx = { ...context };
    const v = this.voice;
    const dt2 = this.device === 'dt2';
    const q = this.q;
    if (this.trig) {
      if (dt2) q.poke(nw(RECORDS + v * REC + 0x1b8), 1);
      else this.trigBit(true);
    }
    if (this.release_) this.releaseByte(1);
    const { regs, out } = this.regs(context);
    for (let k = 0; k < 32; k++) {
      q.poke(nw(out) + k, NAN);
      if (dt2) q.poke(nw(out + 0x80) + k, NAN);
    }
    q.poke(nw(STACK) - 1, RETURN_SW - 1);
    q.poke(nw(STACK), 0x20000ff0);
    // the mask of the registered types, as voice 0 of an earlier block leaves it (the table holds the one row)
    q.poke(nw(this.built.data._dv_mask!), (1 << Math.min(this.spec.machineType, 31)) >>> 0);
    const reads: [number, number][] = [[nw(out), 32]];
    if (dt2) reads.push([nw(out + 0x80), 32]);
    reads.push([nw(TABLE) + 13, 1]);
    if (dt2) reads.push([nw(RECORDS + v * REC + 0x1b8), 1]);
    const firstExtra = reads.length;
    for (const [a, n] of extraReads) reads.push([nw(a), n]);
    q.push({
      call: {
        pc: this.built.symbols._dv_entry!,
        regs,
        max_steps: this.stepLimit,
        reads,
        read_regs: ['I6', 'I7', 'I4', ...CONTEXT_REGS],
        ...(this.profile ? { profile: true } : {}),
      },
    });
    const [res] = (await this.port.run(q.take())) as [CallResult];
    if (this.trig && !dt2) this.trigBit(false);
    if (this.release_) this.releaseByte(0);
    this.trig = this.release_ = false;
    const stop = dt2 ? RETURN_SW : STOCK_BP;
    const returned = res.halt === 'breakpoint' && res.final_pc === stop;
    const faults = res.reads[dt2 ? 2 : 1]?.[0] ?? 0;
    return {
      samples: words(res.reads[0], 32),
      instructions: res.instructions,
      returned,
      halt: res.halt,
      finalPc: res.final_pc,
      contextBad: returned ? contextBad(res.regs, context, res.final_pc) : [],
      faults: faults & 0xffff,
      foreign: this.recordStores ? this.foreignStores(out, res.stores) : [],
      right: dt2 ? words(res.reads[1], 32) : [],
      active: dt2 ? (res.reads[3]?.[0] ?? 0) : 0,
      extra: extraReads.map((_, i) => res.reads[firstExtra + i] ?? []),
      profile: res.profile ?? [],
    };
  }

  /** DT2: the odd voice of the pair, called after `block` the way the engine does: it copies the even voice's block unless the even voice wrote R. Returns its 32 floats (NaN where nothing wrote). */
  async oddBlock(ctx: Context | 'friendly' = 'friendly'): Promise<number[]> {
    if (this.device !== 'dt2') throw new Error('only the Digitakt II has a voice pair');
    const context = resolveContext(ctx, this.ctxRng);
    const v = 2 * this.track + 1;
    const { regs } = this.regs(context);
    const q = this.q;
    q.poke(nw(STACK) - 1, RETURN_SW - 1);
    q.poke(nw(STACK), 0x20000ff0);
    q.push({
      call: {
        pc: this.built.symbols._dv_entry!,
        regs: { ...regs, R4: RECORDS + v * REC, R8: OUT + v * 0x80, R14: 32 - v },
        max_steps: this.stepLimit,
        reads: [[nw(OUT + v * 0x80), 32]],
        read_regs: ['I6', 'I7'],
      },
    });
    const [res] = (await this.port.run(q.take())) as [CallResult];
    if (res.halt !== 'breakpoint') throw new Error(`odd voice call halted: ${res.halt}`);
    return words(res.reads[0], 32);
  }

  // -- the write set --------------------------------------------------------------------------------------------------------

  /**
   * The DM a routine of the module may store to in a call of the current voice (docs/design/module-sdk.md 11.5 D6, <dk/machine/machine.h>):
   * the output block, the voice's own state block (base + stride * track, `state_bytes`), its data stack and its own data and bss (shared by all
   * voices) (`stack_bytes` bytes below the device's actual module-entry I7). A routine without a description (the raw form) has no state; it
   * gets DEFAULT_STACK_BYTES of stack.
   */
  private allowed(outAddr: number): Region[] {
    const b = this.built;
    const out: Region[] = [{ address: outAddr, bytes: 4 * 32, name: 'output block' }];
    if (this.device === 'dt2')
      out.push({ address: outAddr + 0x80, bytes: 4 * 32, name: 'right output block' }); // a stereo machine writes R into the odd voice's block
    else if (b.data._gs_rbuf !== undefined)
      out.push({ address: b.data._gs_rbuf + 128 * this.voice, bytes: 4 * 32, name: 'right output block' }); // DN2: the slot's R buffer
    if (b.desc.length)
      out.push({ address: b.state + b.stride * this.track, bytes: b.desc[4]!, name: `voice ${this.track} state` });
    for (const r of b.moduleData) out.push({ ...r, name: 'module data' }); // shared by all voices
    const frame = ADAPTER_FRAME_WORDS[this.device]!;
    const stack = b.desc.length ? b.desc[7]! : DEFAULT_STACK_BYTES;
    out.push({ address: STACK - 4 * frame - stack, bytes: stack, name: 'module stack' });
    return out;
  }

  /** Regions to name a foreign address by: the other voices' states, the adapter's data and frame, the stand-ins. */
  private names(): Region[] {
    const b = this.built;
    const out: Region[] = b.wrapperData.map((r: Range) => ({ ...r, name: 'adapter data' }));
    if (b.desc.length) {
      for (let t = 0; t < b.desc[3]!; t++)
        out.push({ address: b.state + b.stride * t, bytes: b.desc[4]!, name: `voice ${t} state` });
      out.push({ address: b.state, bytes: b.stride * b.desc[3]!, name: 'voice state array' });
    }
    const frame = ADAPTER_FRAME_WORDS[this.device]!;
    out.push(
      { address: STACK - 4 * frame, bytes: 4 * frame, name: 'adapter frame' },
      { address: FRAME, bytes: 0x1000, name: 'frame copy' },
      { address: RECORDS, bytes: 0x1000, name: 'voice records' },
      { address: TYPE_CACHE, bytes: 0x1000, name: 'type cache' },
      { address: TABLE, bytes: 0x1000, name: 'registry' },
      { address: OUT, bytes: 0x1000, name: 'output buffers' },
      { address: DIAG, bytes: 0x1000, name: 'adapter diagnostics' },
    );
    return out;
  }

  /** Stores of the last call made by the module's code outside `allowed`, as text (at most 4). */
  private foreignStores(outAddr: number, stores: CallResult['stores']): string[] {
    const allowed = this.allowed(outAddr);
    const found: string[] = [];
    for (const [pc, address, width] of stores) {
      if (!this.built.moduleCode.some(([lo, hi]) => lo <= pc && pc < hi)) continue;
      // the core keeps a store to a not yet mapped address at the loader alias (SW_ALIAS_BASE + address)
      const at = address >= SW_ALIAS_BASE ? address - SW_ALIAS_BASE : address;
      if (allowed.some((a) => a.address <= at && at + width <= a.address + a.bytes)) continue;
      found.push(
        `in ${this.callOf(pc)}: store of ${width} bytes to ${hex(at)} at pc ${hex(pc)}, ${this.near(at, allowed)}`,
      );
      if (found.length === 4) break;
    }
    return found;
  }

  /** `note_on` or `render`: the entry symbol at or below the pc (the routines they call are in the same object, after them). */
  private callOf(pc: number): string {
    const sy = this.built.symbols;
    const named: [number, string][] = [[sy[this.spec.entry]!, 'render']];
    if (this.spec.noteOn !== null) named.push([sy[this.spec.noteOn]!, 'note_on']);
    const below = named.filter(([a]) => a <= pc);
    // the highest address, as Python's max over (address, name) pairs
    return below.length ? below.reduce((m, x) => (x[0] > m[0] || (x[0] === m[0] && x[1] > m[1]) ? x : m))[1] : 'render';
  }

  private near(addr: number, allowed: Region[]): string {
    for (const r of this.names())
      if (r.address <= addr && addr < r.address + r.bytes) return `inside ${r.name} +${hex(addr - r.address)}`;
    let best = allowed[0]!;
    let bestGap = Infinity;
    for (const r of allowed) {
      const d = Math.min(Math.abs(addr - r.address), Math.abs(addr - (r.address + r.bytes)));
      if (d < bestGap) [best, bestGap] = [r, d];
    }
    const gap = addr >= best.address + best.bytes ? addr - (best.address + best.bytes) : best.address - addr;
    if (gap <= 64)
      return `${gap} bytes ${addr >= best.address + best.bytes ? 'past the end of' : 'before'} its own ${best.name}`;
    return 'unknown memory';
  }
}

interface Region {
  address: number;
  bytes: number;
  name: string;
}
