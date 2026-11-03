import fs from 'node:fs';
import path from 'node:path';
import { repoRoot, type Device } from '@digi/test-support';
import { parse } from 'smol-toml';
import { deviceProfile } from './device-data.ts';
import { patchkit } from './patchkit.ts';
import { planSymbol, type RuntimePlan } from './runtime-plan.ts';

/**
 * Synthetic `.dkm` files: `patchkit synth-dkm` (packages/patchkit/src/dkmsynth.rs) builds a module of a chosen size, with a chosen number of
 * machines, parameters, pages, DSP bytes and voices from a TOML spec. This file has the typed spec, its TOML text, the packing and the geometry
 * of a device that a set of such modules is computed from. It holds no test logic.
 */

/** A voice row of the DSP section: a pinned machine type number, or a machine of the file (its hashed key). */
export interface SynthVoice {
  type?: number;
  machine?: string;
  stateBytes?: number;
  voices?: number;
}

/** The DSP section: `image` and `bss` are bytes (multiples of 4), `align` is 8, 16, 32 or 64. */
export interface SynthDsp {
  image: number;
  bss?: number;
  align?: number;
  voices?: SynthVoice[];
}

/** The fields of `dkmsynth::Spec`. */
export interface SynthSpec {
  name: string;
  /** Data bytes behind the code (a multiple of 4): makes the file larger. */
  pad?: number;
  /** The byte of `pad` and of the DSP image. */
  fill?: number;
  /** ColdFire bss bytes: not stored in the file. */
  bss?: number;
  machines?: number;
  params?: number;
  pages?: number;
  /** Allow a file over the staging buffer. */
  oversize?: boolean;
  dsp?: SynthDsp;
}

/** The TOML text of SPEC. */
export function synthToml(spec: SynthSpec): string {
  const lines = [`name = ${JSON.stringify(spec.name)}`];
  for (const key of ['pad', 'bss', 'machines', 'params', 'pages'] as const)
    if (spec[key]) lines.push(`${key} = ${spec[key]}`);
  if (spec.fill !== undefined) lines.push(`fill = ${spec.fill}`);
  if (spec.oversize) lines.push('oversize = true');
  if (spec.dsp) {
    lines.push('[dsp]', `image = ${spec.dsp.image}`);
    if (spec.dsp.bss) lines.push(`bss = ${spec.dsp.bss}`);
    if (spec.dsp.align) lines.push(`align = ${spec.dsp.align}`);
    for (const v of spec.dsp.voices ?? []) {
      lines.push('[[dsp.voice]]');
      if (v.type !== undefined) lines.push(`type = ${v.type}`);
      if (v.machine !== undefined) lines.push(`machine = ${JSON.stringify(v.machine)}`);
      if (v.stateBytes) lines.push(`state_bytes = ${v.stateBytes}`);
      if (v.voices) lines.push(`voices = ${v.voices}`);
    }
  }
  return `${lines.join('\n')}\n`;
}

/** Pack SPEC for DEVICE into DIR/FILE (default: the spec's name with `/` as `-`); returns the path. */
export async function packSynth(device: Device, spec: SynthSpec, dir: string, file?: string): Promise<string> {
  fs.mkdirSync(dir, { recursive: true });
  const out = path.join(dir, file ?? `${spec.name.replace(/\//g, '-')}.dkm`);
  const toml = `${out}.toml`;
  fs.writeFileSync(toml, synthToml(spec));
  await patchkit(['synth-dkm', toml, '--device', device, '-o', out]);
  return out;
}

/** The voices that name machine types by number (a DSP-only module: no machine record, no hashed key). */
export const pinned = (types: number[]): SynthVoice[] => types.map((type) => ({ type }));

/** The loader's format limits (sdk/sdk.toml `[limits.format]`, tools/device/dkproto.py). */
export const ARENA_HDR = 16; // the arena block's header
export const DSP_GRANULE = 64; // the DSP area is handed out in granules
export const DKM_HEADER = 160;
export const MAX_VOICES = 8; // voices of one DSP section
/** The synthetic module's code: `moveq #0,d0; rts`. */
export const CODE = 4;

/** What a set of synthetic modules is computed from: the device's limits, its run-time machine type range and the arena and DSP area addresses. */
export interface Geometry {
  device: Device;
  arenaLo: number;
  arenaHi: number;
  dspArea: number;
  dspBase: number;
  stageBytes: number;
  arenaBytes: number;
  modules: number;
  machineRows: number;
  paramRows: number;
  pageRows: number;
  typeFirst: number;
  /** Offset in the DSP area of the first 8-aligned byte (the area's base is not 8-aligned on the Digitakt II). */
  dspFirst: number;
  /** Bytes of the arena's one free block at the start (header included). */
  arenaFree: number;
}

/** `dynamic_first ..= last` of the device in the machines `types.toml`: the run-time machine type numbers. */
export function dynamicRange(device: Device): [number, number] {
  const types = parse(
    fs.readFileSync(path.join(repoRoot(), 'modules/core/adapters/machines/types.toml'), 'utf8'),
  ) as Record<string, { dynamic_first: number; last: number }>;
  return [types[device]!.dynamic_first, types[device]!.last];
}

/** The plan must carry the symbols `DKRT_ARENA` (core/runtime) and `_dl_area` (core/dsp-link). */
export function geometryOf(device: Device, plan: RuntimePlan): Geometry {
  const profile = deviceProfile(device);
  const lim = profile.limits;
  const [first, last] = dynamicRange(device);
  if (last - first + 1 !== lim.machine_rows!)
    throw new Error(`${device}: machine_rows ${lim.machine_rows} is not the run-time type range ${first}..${last}`);
  const arenaBase = planSymbol(plan, 'DKRT_ARENA');
  const dspBase = planSymbol(plan, '_dl_area');
  const arenaLo = (arenaBase + 15) & ~15;
  const arenaHi = (arenaBase + lim.arena_bytes!) & ~15;
  return {
    device,
    arenaLo,
    arenaHi,
    dspArea: profile.digikit.dsp_area_bytes,
    dspBase,
    stageBytes: lim.stage_bytes!,
    arenaBytes: lim.arena_bytes!,
    modules: lim.modules!,
    machineRows: lim.machine_rows!,
    paramRows: lim.param_rows!,
    pageRows: lim.page_rows!,
    typeFirst: first,
    dspFirst: -dspBase & 7,
    arenaFree: arenaHi - arenaLo,
  };
}

/** The arena bytes a module of image + bss = N takes: its payload rounded to 16 and the header. */
export const block = (n: number): number => ((n + 15) & ~15) + ARENA_HDR;

/** `pad` of a raw module whose file is `stageBytes + extra` bytes (the file is the header, the code and the pad). */
export const stageFilePad = (stageBytes: number, extra = 0): number => stageBytes + extra - DKM_HEADER - CODE;

// ---- the full set: every limit used at once (capacity.device.test.ts, memaudit.device.test.ts) ---------------------------------

export const E_NOMEM = -8;
export const E_DSP_NOSPACE = -33;
export const E_DSP_ROWS = -34;
export const LOADED = 'loaded';
/** The result a module must give at boot: loaded, or the loader's error code. */
export type Want = number | typeof LOADED;

/** What a packed module takes of each resource (`patchkit needs`). */
export interface Usage {
  arena: number;
  arenaXfer: number;
  dspLen: number;
  dspAlign: number;
  rows: number;
  machines: number;
  params: number;
  pages: number;
}

export function usageOf(n: Record<string, number>): Usage {
  return {
    arena: n.ARENA!,
    arenaXfer: n.ARENA_XFER!,
    dspLen: n.DSP_AREA!,
    dspAlign: n.DSP_ALIGN!,
    rows: n.VOICE_ROWS!,
    machines: n.MACHINE_ROWS!,
    params: n.PARAM_ROWS!,
    pages: n.PAGE_ROWS!,
  };
}

/**
 * The modules that, behind the reference modules REFS (loaded first), use every limit at once, followed by three that do not fit:
 * M takes the machine types, parameter rows and page rows the references leave; V the voice rows (pinned types, behind the references' hashed ones);
 * D the DSP area, to its last whole granule; F the arena, to the last byte (the references' and V's and D's blocks and the arena copies of their DSP
 * images counted: a ColdFire-only rig keeps those until a transfer that never comes). Then X (a raw module: E_NOMEM), Y (a voice: E_DSP_ROWS) and
 * Z (a DSP module: E_DSP_NOSPACE).
 */
export function fullSet(g: Geometry, refs: Usage[]): { spec: SynthSpec; want: Want }[] {
  const sum = (key: keyof Usage) => refs.reduce((n, r) => n + r[key], 0);
  const typesLeft = g.machineRows - sum('machines');
  const rowsLeft = g.machineRows - sum('rows');
  const paramsLeft = g.paramRows - sum('params');
  const pagesLeft = g.pageRows - sum('pages');
  if (refs.length + 7 > g.modules)
    throw new Error(`the full set needs ${refs.length + 7} modules, the device has ${g.modules}`);
  if (rowsLeft > MAX_VOICES || rowsLeft < 1 || typesLeft < 1)
    throw new Error(`the references leave ${rowsLeft} rows and ${typesLeft} types`);
  if (pagesLeft > typesLeft)
    throw new Error(`${pagesLeft} page rows left, but only ${typesLeft} machines to put them on`);
  const firstPinned = g.typeFirst + sum('rows');
  // the DSP area: references, then V (one granule), then D takes every whole granule left (first fit from the area's start, each range at the
  // first address its alignment allows)
  let cursor = 0;
  for (const [length, align] of [
    ...refs.filter((r) => r.dspLen).map((r) => [r.dspLen, r.dspAlign] as const),
    [DSP_GRANULE, 8] as const,
  ]) {
    const start = Math.ceil((g.dspBase + cursor) / align) * align - g.dspBase;
    cursor = start + length;
  }
  const dStart = Math.ceil((g.dspBase + cursor) / 8) * 8 - g.dspBase;
  const dLen = Math.floor((g.dspArea - dStart) / DSP_GRANULE) * DSP_GRANULE;
  if (dLen < DSP_GRANULE) throw new Error('the references leave no DSP area for D');
  // every raw block is `block(CODE)`; V's and D's DSP images keep an arena copy of `block(64)` each (as does every reference's)
  const used = refs.reduce((n, r) => n + r.arena + r.arenaXfer, 0) + 3 * block(CODE) + 2 * block(DSP_GRANULE);
  const fTotal = g.arenaFree - used - 16;
  if (fTotal < CODE) throw new Error('the references leave no arena for F');
  return [
    { spec: { name: 'cap/full-m', machines: typesLeft, params: paramsLeft, pages: pagesLeft }, want: LOADED },
    {
      spec: {
        name: 'cap/full-v',
        dsp: { image: 64, voices: pinned(Array.from({ length: rowsLeft }, (_, i) => firstPinned + i)) },
      },
      want: LOADED,
    },
    { spec: { name: 'cap/full-d', dsp: { image: 64, bss: dLen - 64 } }, want: LOADED },
    { spec: { name: 'cap/full-f', bss: fTotal - CODE }, want: LOADED },
    { spec: { name: 'cap/full-x' }, want: E_NOMEM },
    { spec: { name: 'cap/full-y', dsp: { image: 64, voices: pinned([firstPinned]) } }, want: E_DSP_ROWS },
    { spec: { name: 'cap/full-z', dsp: { image: 64 } }, want: E_DSP_NOSPACE },
  ];
}
