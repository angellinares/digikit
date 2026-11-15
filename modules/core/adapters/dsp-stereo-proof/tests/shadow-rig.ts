/**
 * The rig of the shadow replay (port of the fixtures of tests/patchkit/test_dsp_stereo_shadow_replay.py): the stock DN2 1.12 DSP image as
 * the base core, one synthetic object of tests/fixtures/dsp/stereo-*-shadow-replay.s linked beside it, and every call watched for stores
 * of all code. Addresses are the normal-word addresses of the Python test; `Shadow` converts them to the byte addresses of the kit.
 * Original firmware is read at run time, never copied into a fixture.
 */
import { readFileSync } from 'node:fs';
import path from 'node:path';
import {
  ROOT,
  assemble,
  bits,
  expandIncludes,
  link,
  openImageSession,
  type CallResult,
  type Linked,
  type StereoSession,
} from '@digi/sharc-rig-kit';

export { bits };
export const BACK = 0xb9f000;
export const STACK = 0x08044000;
export const CONTROL = 0x08040000;
export const FIRST = 0x08040200;
export const UY = 0x08040400;
export const LEFT = 0x08041000;
export const RIGHT = 0x08041100;

/** the context of the Python test: byte frame pointers when the fixture needs them are `STACK * 4` */
export const REGS: Record<string, number> = (() => {
  const r: Record<string, number> = { I6: STACK, I7: STACK, M5: 0, M6: 1, M7: 0xffffffff, M14: 1, MODE1: 0 };
  for (let i = 0; i < 16; i++) r[`L${i}`] = 0;
  return r;
})();

/** `normal_word_to_byte` for the ranges the replay uses (L2 and DDR) */
export function toByte(address: number): number {
  if (address >= 0x08000000 && address < 0x08046000) return address * 4;
  if (address >= 0x10000000 && address < 0x18000000) return 0x80000000 + (address - 0x10000000) * 4;
  throw new Error(`no byte address for ${address.toString(16)}`);
}

/** the address of a store as the core reports it: L2 at the loader's alias, DDR (0x80000000 and up) as it is */
export const canonical = (byteAddress: number): number => (byteAddress >= 0x80000000 ? byteAddress : (0x28000000 + byteAddress) >>> 0);

export type Store = [pc: number, address: number, width: number];

const sdk = (): string[] => [path.join(ROOT, 'sdk')];
const fixture = (name: string): string => readFileSync(path.join(ROOT, 'tests/fixtures/dsp', name), 'utf8');

export interface ShadowBuild {
  objects: Linked[];
  symbols: Record<string, number>;
}

const builds = new Map<string, ShadowBuild>();

/** The fixtures `files` (concatenated, as the Python test does for the filter and the prepared stages) linked at `base` against the stock functions `stock`. */
export function buildShadow(key: string, files: string[], base: number, stock: Record<string, number>, more: ShadowBuild[] = []): ShadowBuild {
  const known = builds.get(key);
  if (known) return known;
  const source = expandIncludes(files.map(fixture).join('\n'), sdk());
  const linked = link(assemble(source), base, stock);
  const build = {
    objects: [linked, ...more.flatMap((m) => m.objects)],
    symbols: { ...Object.assign({}, ...more.map((m) => m.symbols)), ...linked.symbols },
  };
  builds.set(key, build);
  return build;
}

export interface CallOptions {
  maxSteps?: number;
  readRegs?: string[];
  breaks?: number[];
  resume?: boolean;
  profile?: boolean;
}

/** how often the call executed the instructions at `pcs` (the rig does not record loads: a load that runs once reads once) */
export const executed = (r: CallResult, pcs: readonly number[]): number =>
  (r.profile ?? []).filter(([pc]) => pcs.includes(pc)).reduce((n, [, c]) => n + c, 0);

/** The rig reports the bytes one instruction stored as runs: a long-word or SIMD store of two words is one run of 8 bytes. The Python core logs one event per word. */
export function wordStores(runs: Store[]): Store[] {
  return runs.flatMap(([pc, address, width]): Store[] =>
    width > 4 && width % 4 === 0 ? Array.from({ length: width / 4 }, (_, k) => [pc, address + 4 * k, 4] as Store) : [[pc, address, width]],
  );
}

export class Shadow {
  /** the stores of the last call */
  stores: Store[] = [];
  readonly raw: StereoSession;
  private readonly symbols: Record<string, number>;

  private constructor(raw: StereoSession, symbols: Record<string, number>) {
    this.raw = raw;
    this.symbols = symbols;
  }

  static async open(build: ShadowBuild): Promise<Shadow> {
    const s = await openImageSession({ code: build.objects, poison: false, watch: [[0, 0x1000000]] });
    return new Shadow(s, build.symbols);
  }

  close(): Promise<void> {
    return this.raw.close();
  }

  pc(entry: string): number {
    const pc = this.symbols[entry];
    if (pc === undefined) throw new Error(`no symbol ${entry}`);
    return pc;
  }

  poke(address: number, word: number): void {
    this.raw.poke(toByte(address), word);
  }

  words(address: number, count: number): Promise<number[]> {
    return this.raw.words(toByte(address), count);
  }

  async peek(address: number): Promise<number> {
    return (await this.words(address, 1))[0]!;
  }

  /** Python's `call`: runs `entry` (a symbol or a pc) to a break; the caller checks the halt. */
  async call(entry: string | number, regs: Record<string, number>, o: CallOptions = {}): Promise<CallResult> {
    const pc = typeof entry === 'number' ? entry : this.pc(entry);
    const result = await this.raw.raw(pc, regs, {
      maxSteps: o.maxSteps ?? 12000,
      readRegs: o.readRegs,
      breaks: o.breaks,
      resume: o.resume,
      profile: o.profile,
    });
    this.stores = wordStores(result.stores);
    return result;
  }
}

export const words32 = (n: number): number[] => Array.from({ length: n }, (_, i) => i);
