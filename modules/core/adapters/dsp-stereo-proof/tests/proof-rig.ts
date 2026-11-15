/**
 * The rig of the private DN2 1.12 stereo proof observers (port of the fixtures of test_dsp_stereo_proof.py and test_dsp_stereo_uy_proof.py): the
 * stock DSP image as the base core, `stereo-proof.s` (and `uy-proof.s`) linked at 0xb90000 with its private data at 0x200d0000, the stock
 * render and reset of the image as the observed functions. Original firmware is read at run time, never copied into a fixture.
 */
import { readFileSync } from 'node:fs';
import path from 'node:path';
import {
  BACK,
  CALLER,
  PRIVATE,
  ROOT,
  STEREO_FRAME as FRAME,
  assemble,
  bits,
  expandIncludes,
  link,
  openImageSession,
  placeObjectData,
  type CallResult,
  type Linked,
  type StereoSession,
} from '@digi/sharc-rig-kit';

export { BACK, CALLER, FRAME, PRIVATE, bits };
export const MOD = path.resolve(import.meta.dirname, '..');
export const STATE = 0x24f320;
export const INPUT = 0x20100000;
export const OUTPUT = 0x20100200;
export const ENGINE_BASE = 0x241298;

/** stock functions of the image and the entries the proof tests call, by the names of the Python tests */
export const STOCK_PC: Record<string, number> = {
  stock_render: 0xb80c39,
  stock_reset: 0xb80b93,
  uy_stock_render: 0xb81368,
  uy_stock_reset: 0xb8135a,
};

export interface ProofBuild {
  linked: Linked;
  /** data symbol -> byte address */
  syms: Record<string, number>;
  inits: { address: number; bytes: Uint8Array; bss: boolean }[];
  /** end of the private data */
  end: number;
  /** code symbols and the stock entries, by name */
  pc: Record<string, number>;
  parcels: number;
  /** the object of the first stage alone is a prefix of the linked code (the uy build only) */
  prefixHolds: boolean;
}

const globalize = (text: string, labels: string[]): string =>
  labels.reduce((t, l) => t.replaceAll(`${l}:`, `.GLOBAL ${l};\n${l}:`), text);
const builds = new Map<boolean, ProofBuild>();

/** The first stage alone (`uy` false), or the first stage with U/Y appended (`uy` true). */
export function buildProof(uy: boolean): ProofBuild {
  const known = builds.get(uy);
  if (known) return known;
  const sdk = [path.join(ROOT, 'sdk')];
  const base = readFileSync(path.join(MOD, 'src/dsp/stereo-proof.s'), 'utf8');
  const uyText = readFileSync(path.join(MOD, 'src/dsp/uy-proof.s'), 'utf8');
  // export a local boundary to the model fixture without changing instructions or the production object's public symbol table
  const source = uy
    ? `${base}\n${globalize(uyText, ['_uy_right_start', '_uy_render_finish'])}`
    : globalize(base, ['_sp_render_finish', '_sp_right_start']);
  const obj = assemble(expandIncludes(source, sdk));
  const { data, inits, end } = placeObjectData(obj, PRIVATE);
  const stock = { _sp_stock_render: 0xb80c39, _sp_stock_reset: 0xb80b93, _sp_engine_base: ENGINE_BASE };
  const uyStock = { _uy_stock_render: 0xb81368, _uy_stock_reset: 0xb8135a, _uy_engine_base: ENGINE_BASE };
  const bindings = { ...data, ...stock, ...(uy ? uyStock : {}) };
  const linked = link(obj, 0xb90000, bindings);
  let prefixHolds = true;
  if (uy) {
    // appending U/Y must retain every original first-stage instruction/relocation
    const alone = link(assemble(expandIncludes(base, sdk)), 0xb90000, bindings);
    prefixHolds = linked.codeBoot.startsWith(alone.codeBoot);
  }
  const b: ProofBuild = {
    linked,
    syms: data,
    inits,
    end,
    pc: { ...STOCK_PC, ...linked.symbols },
    parcels: obj.parcels,
    prefixHolds,
  };
  builds.set(uy, b);
  return b;
}

/** A session with the proof object over the image; the frame the stock functions are called with (count 32) is built. `mode` is `_sp_mode` (and `_uy_*`'s via the same word). */
export async function fresh(b: ProofBuild, mode?: number): Promise<StereoSession> {
  const pokes: [number, number][] = [];
  for (let a = FRAME - 512; a < FRAME + 20; a += 4) pokes.push([a, 0]);
  pokes.push([FRAME, CALLER], [FRAME - 4, BACK - 1], [FRAME + 4, 32]);
  const s = await openImageSession({
    code: [b.linked],
    inits: b.inits,
    poison: false,
    pokes,
    watch: [[0, 0x1000000]],
  });
  if (mode !== undefined) s.poke(b.syms._sp_mode!, mode);
  return s;
}

export function seed(s: StereoSession, slot = 0): number {
  const state = STATE + 112 * slot;
  for (let i = 0; i < 28; i++) s.poke(state + i * 4, 0);
  for (const [i, v] of Object.entries({ 21: 0.7, 22: 0.2, 23: 0.1, 24: 0.1, 25: -0.05, 26: 0.3, 27: -0.2 })) s.poke(state + Number(i) * 4, bits(v));
  return state;
}

export const relationWords = (words: readonly number[]): number[] => words.map((w) => ((w & 0x7fffffff) === 0 ? 0 : w >>> 0));
export const values = (s: StereoSession, start: number, count: number): Promise<number[]> => s.words(start, count);

/** A peeked word that the test knows was written. */
export async function peekWord(s: StereoSession, address: number): Promise<number> {
  const w = await s.peek(address);
  if (w === null) throw new Error(`DM word ${address.toString(16)} was never written`);
  return w;
}

/** The context the Python tests call with: every register distinct, the DAG registers as the engine keeps them. */
export function regs(state: number, extra: Record<string, number> = {}): Record<string, number> {
  const r: Record<string, number> = {};
  for (let i = 0; i < 16; i++) {
    r[`R${i}`] = 0x1100 + i;
    r[`I${i}`] = 0x200c0000 + 256 * i;
    r[`L${i}`] = 0;
    r[`B${i}`] = 0x200c0000 + 256 * i;
    r[`M${i}`] = 0;
  }
  Object.assign(r, {
    M5: 0,
    M6: 1,
    M7: 0xffffffff,
    M14: 1,
    I6: FRAME,
    I7: FRAME - 8,
    R2: CALLER,
    R4: state,
    R8: INPUT,
    R12: OUTPUT,
    MODE1: 0x10000,
    ASTATX: 0x40000,
    ASTATY: 0x12345,
    LCNTR: 11,
  });
  for (let i = 0; i < 16; i++) r[`S${i}`] = 0x3f010000 + 17 * i;
  return { ...r, ...extra };
}

/** One full call (to the return point), with the frame checked afterwards; the stores of all code come back in `stores`. */
export async function call(
  s: StereoSession,
  entry: number,
  registers: Record<string, number>,
  limit = 12000,
): Promise<CallResult> {
  const result = await s.raw(entry, registers, { maxSteps: limit, readRegs: Object.keys(registers) });
  if (result.halt !== 'breakpoint' || result.final_pc !== BACK) throw new Error(`call did not return: ${JSON.stringify({ ...result, stores: result.stores.length })}`);
  if ((await s.peek(FRAME)) !== CALLER) throw new Error('the caller frame word changed');
  if ((await s.peek(FRAME - 4)) !== BACK - 1) throw new Error('the return word changed');
  return result;
}

/** The core's address of a DM byte address as the stores report it: every byte of the frame, the L2 words and the firmware's data at the loader's alias (0x28000000 + address). */
export const canonical = (address: number): number => (0x28000000 + address) >>> 0;

export const u32 = (n: number): number => n >>> 0;
