/**
 * A rig for plain routines: assembled objects placed at the bases the test names, their data in free L2, stand-in addresses as
 * externs, and calls with registers set by the test (port of the `CallSession` use of the adapter tests, `pk_sharc.CallSession`).
 * There is no machine or effect around it: the test pokes the words a routine reads and reads the words it writes.
 *
 *     const rig = await RawRig.open({ objects: [{ text, base: 0xb90000 }], externs: { _fw_sat: STOCK }, returnSw: RET, breaks: [STOCK] });
 *     rig.poke(addr / 4, word);
 *     const r = await rig.call('_od_sat', { R4: REC }, { reads: [[OUT / 4, 32]] });
 */
import { OpQueue, realRig, type RigOpener } from './ops.ts';
import { DATA_BASE } from './place.ts';
import type { CallResult, RigPort } from './rig.ts';
import { SW_ALIAS_BASE, assemble, expandIncludes, link, type SharcObject } from './toolchain.ts';

export interface RawObject {
  /** the source text; `includes` expand its `#include` lines */
  text: string;
  includes?: string[];
  /** short-word address of the code */
  base: number;
}

export interface RawOptions {
  objects: RawObject[];
  /** stand-in symbols (name -> address); the data symbols and the code symbols of the objects are added */
  externs?: Record<string, number>;
  returnSw: number;
  breaks?: number[];
  /** byte address of the first data run (default: the free L2 the machine rigs use) */
  dataBase?: number;
  open?: RigOpener;
}

export interface RawCall {
  maxSteps?: number;
  reads?: [address: number, count: number][];
  readRegs?: string[];
}

/** Data runs in order, each section at its alignment (`align` of the assembler, else 4 bytes); a data symbol is its run plus its offset. */
export function placeAligned(
  objects: SharcObject[],
  start: number,
): { data: Record<string, number>; inits: { address: number; bytes: Uint8Array }[] } {
  let address = start;
  const data: Record<string, number> = {};
  const inits: { address: number; bytes: Uint8Array }[] = [];
  for (const o of objects) {
    const where: Record<string, number> = {};
    for (const [section, { bytes, align }] of Object.entries(o.data)) {
      const a = Math.max(align ?? 4, 1);
      address = Math.ceil(address / a) * a;
      where[section] = address;
      inits.push({ address, bytes });
      address += bytes.length;
    }
    for (const [name, { section, offset }] of Object.entries(o.dataSymbols)) data[name] = where[section]! + offset;
  }
  return { data, inits };
}

export class RawRig {
  /** data symbol -> byte address */
  readonly data: Record<string, number>;
  /** code symbol -> short-word address */
  readonly symbols: Record<string, number>;
  private readonly q = new OpQueue();
  private port!: RigPort;
  private returnSw = 0;

  private constructor(data: Record<string, number>, symbols: Record<string, number>) {
    this.data = data;
    this.symbols = symbols;
  }

  static async open(o: RawOptions): Promise<RawRig> {
    const objs = o.objects.map((x) => assemble(expandIncludes(x.text, x.includes ?? [])));
    const { data, inits } = placeAligned(objs, o.dataBase ?? DATA_BASE);
    const symbols: Record<string, number> = {};
    objs.forEach((obj, i) => {
      for (const [name, offset] of Object.entries(obj.symbols))
        if (!(name in data)) symbols[name] = o.objects[i]!.base + offset;
    });
    const externs = { ...o.externs, ...data, ...symbols };
    const linked = objs.map((obj, i) => link(obj, o.objects[i]!.base, externs));
    const rig = new RawRig(data, symbols);
    rig.returnSw = o.returnSw;
    for (const { address, bytes } of inits) {
      const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
      for (let k = 0; k < Math.floor(bytes.length / 4); k++) rig.q.poke(address / 4 + k, view.getUint32(4 * k, true));
    }
    const { port } = await (o.open ?? realRig)({
      return_sw: o.returnSw,
      breaks: o.breaks ?? [],
      code: linked.map((l) => ({ address: SW_ALIAS_BASE + 2 * l.baseSw, hex: l.codeBoot })),
      ops: rig.q.take(),
    });
    rig.port = port;
    return rig;
  }

  async close(): Promise<void> {
    await this.port.close();
  }

  /** Write a DM word (a normal-word address; a core MMR is its own address). Takes effect before the next call or peek. */
  poke(wordAddress: number, word: number): void {
    this.q.poke(wordAddress, word);
  }

  /** The words at `wordAddress` now (null for a word that nothing wrote). Pending pokes are sent first. */
  async peek(wordAddress: number, count = 1): Promise<(number | null)[]> {
    return (await this.peekMany([[wordAddress, count]]))[0] ?? [];
  }

  /** `peek` of several ranges in one round trip. */
  async peekMany(ranges: [address: number, count: number][]): Promise<(number | null)[][]> {
    const r = await this.call(this.returnSw, {}, { maxSteps: 0, reads: ranges });
    return r.reads;
  }

  /** Call a routine (a symbol or a short-word address) with `regs` set; every other register stays as the last call left it. */
  async call(routine: string | number, regs: Record<string, number> = {}, o: RawCall = {}): Promise<CallResult> {
    const pc = typeof routine === 'number' ? routine : this.symbols[routine];
    if (pc === undefined) throw new Error(`no code symbol ${routine}`);
    this.q.push({
      call: {
        pc,
        regs,
        max_steps: o.maxSteps ?? 400_000,
        reads: o.reads ?? [],
        read_regs: o.readRegs ?? [],
      },
    });
    const results = await this.port.run(this.q.take());
    return results.at(-1)!;
  }
}
