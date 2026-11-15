/**
 * Placing sections: the data of every object to free L2, the code of the objects one after the other from 0xB90000, the stand-ins as
 * externs, and the link (port of `_build` in tests/support/machine_rig.py and fx_rig.py). The result holds what a rig needs: the
 * linked code, the data symbols, the initial bytes of every data run, and the ranges of the module's own code and data.
 */
import { existsSync } from 'node:fs';
import path from 'node:path';
import { STOCK_BP, TABLE, TYPE_CACHE, FRAME, PARAM_REC, RELEASE, UNPACK_BP } from './context.ts';
import { ROOT } from './rig.ts';
import { ConformError, type MachineSpec } from './spec.ts';
import { assembleSource, link, sharcIncludeDirs, type Linked, type SharcObject } from './toolchain.ts';

export const CODE_BASE = 0xb90000;
export const DATA_BASE = 0x200a0000; // byte address of the first data symbol (free L2)
export const DATA_END = 0x20100000;

/** The adapter's sources of a device, assembled as separate objects that see each other's data symbols. */
const DSP = path.join(ROOT, 'modules/core/adapters/dsp-voice/src/dsp');
const WRAPPER: Record<string, string[]> = {
  dt2: [path.join(DSP, 'dsp-voice.s')],
  dn2: [path.join(DSP, 'dsp-voice-dn2.s'), path.join(DSP, 'dsp-voice-dn2-stereo.s')],
};
const FX_DSP = path.join(ROOT, 'modules/core/adapters/fx/src/dsp');
const FX_ADAPTER = path.join(FX_DSP, 'fx.s');
const FX_SEND: Record<string, string> = { dt2: path.join(FX_DSP, 'fxsend.s'), dn2: path.join(FX_DSP, 'fxsend-dn2.s') };

/** The stock functions and sites the stereo hooks of the DN2 adapter name; the machine rig does not run them, the stand-ins only have to link. */
const STEREO_STANDINS = [
  '_gs_fo_stock',
  '_gs_fo_reset_stock',
  '_gs_uy_stock',
  '_gs_uy_reset_stock',
  '_gs_gain_stock',
  '_gs_c5_stock',
  '_gs_held_stock',
  '_gs_shaper_stock',
  '_gs_case1_stock',
  '_gs_case2_stock',
  '_gs_case3_stock',
  '_gs_ring_stock',
  '_gs_held_reset_stock',
  '_gs_case1_reset_stock',
  '_gs_case2_reset_stock',
  '_gs_case3_reset_stock',
  '_gs_ring_reset_stock',
  '_gs_case5_reset_stock',
  '_gs_engine_base',
  '_gs_pan_cont',
  '_gs_pan_end',
  '_gs_pre_cont',
];

/** The bytes of a data run at its address; `bss` runs start poisoned. */
export interface Init {
  address: number;
  bytes: Uint8Array;
  bss: boolean;
}

/** (byte address, bytes) of a data run. */
export interface Range {
  address: number;
  bytes: number;
}

interface DataLayout {
  data: Record<string, number>;
  inits: Init[];
  /** per object: its data runs */
  own: Range[][];
  end: number;
}

/** One 4-byte aligned run per data section, in L2 from `start`; data symbols get the address of their run plus offset. */
export function placeData(objs: SharcObject[], start: number): DataLayout {
  let addr = start;
  const data: Record<string, number> = {};
  const inits: Init[] = [];
  const own: Range[][] = [];
  for (const o of objs) {
    const where: Record<string, number> = {};
    own.push([]);
    for (const [section, { bytes, bss }] of Object.entries(o.data)) {
      where[section] = addr;
      inits.push({ address: addr, bytes, bss });
      own.at(-1)!.push({ address: addr, bytes: bytes.length });
      addr += (bytes.length + 3) & ~3;
    }
    for (const [name, { section, offset }] of Object.entries(o.dataSymbols)) data[name] = where[section]! + offset;
  }
  return { data, inits, own, end: addr };
}

/** The base of each object's code: one after the other, each rounded up to 0x100 short words. Returns the bases and the end. */
export function placeCode(objs: SharcObject[]): { bases: number[]; end: number } {
  const bases: number[] = [];
  let base = CODE_BASE;
  for (const o of objs) {
    bases.push(base);
    base += (o.parcels + 0xff) & ~0xff;
  }
  return { bases, end: base };
}

function linkAll(
  objs: SharcObject[],
  bases: number[],
  externs: Record<string, number>,
): { linked: Linked[]; symbols: Record<string, number> } {
  const symbols: Record<string, number> = {};
  objs.forEach((o, i) => {
    for (const [name, offset] of Object.entries(o.symbols)) symbols[name] = bases[i]! + offset;
  });
  const ext = { ...externs, ...symbols };
  return { linked: objs.map((o, i) => link(o, bases[i]!, ext)), symbols };
}

/** The directories of an object of module MODDIR: its own and the shared ones, then the source's directory. */
function includeDirs(moddir: string): string[] {
  return sharcIncludeDirs(moddir);
}

/** The module directory around a source file (the nearest `module.toml`), if any. */
function moduleAround(file: string): string | null {
  for (let dir = path.dirname(file); dir !== path.dirname(dir); dir = path.dirname(dir))
    if (existsSync(path.join(dir, 'module.toml'))) return dir;
  return null;
}

export interface MachineBuilt {
  linked: Linked[];
  /** data symbol -> byte address */
  data: Record<string, number>;
  inits: Init[];
  /** code symbol -> short-word address */
  symbols: Record<string, number>;
  /** contract voice: byte address of voice 0's state block (the rig reserves it behind the data symbols) */
  state: number;
  /** bytes between two voices' states */
  stride: number;
  /** bytes the adapter zeroes at note-on */
  clear: number;
  /** the module's `dk_machine_desc` words */
  desc: number[];
  /** (first pc, end pc) of each module object (not the wrapper), short words */
  moduleCode: [number, number][];
  /** the module's own data and bss */
  moduleData: Range[];
  /** the adapter's data */
  wrapperData: Range[];
}

function externsFixed(device: string): Record<string, number> {
  const fixed: Record<string, number> = {
    _fw_type_cache: TYPE_CACHE,
    _fw_render_next: STOCK_BP,
    _fw_frame: FRAME,
    _fw_param_rec: PARAM_REC,
    _fw_release_latch: RELEASE,
    _fw_unpack: UNPACK_BP,
    _dv_table: TABLE,
  };
  if (device === 'dn2') STEREO_STANDINS.forEach((n, i) => (fixed[n] = STOCK_BP + 0x10 + 4 * i));
  return fixed;
}

const built = new Map<string, MachineBuilt>();

/** Assemble, place and link the wrapper and the module's DSP sources (cached per spec). */
export function buildMachine(sp: MachineSpec): MachineBuilt {
  const key = JSON.stringify([sp.module, sp.device, sp.sources, sp.entry, sp.noteOn, sp.desc]);
  let b = built.get(key);
  if (!b) built.set(key, (b = buildMachineUncached(sp)));
  return b;
}

function buildMachineUncached(sp: MachineSpec): MachineBuilt {
  const wrappers = WRAPPER[sp.device]!;
  const nwrap = wrappers.length;
  const objs = [
    ...wrappers.map((w) => assembleSource(w, includeDirs(path.resolve(path.dirname(w), '../..')))),
    ...sp.sources.map((s) => assembleSource(s, includeDirs(sp.moddir))),
  ];
  const layout = placeData(objs, DATA_BASE);
  let { end: addr } = layout;
  const { data, inits } = layout;
  // a contract voice: the module's description, and its state blocks behind the data symbols (what the runtime reserves at load)
  let desc: number[] = [];
  let state = 0;
  let stride = 0;
  let clear = 0;
  if (sp.desc !== null) {
    const at = data[sp.desc];
    if (at === undefined) throw new ConformError(`${sp.module}: no data symbol ${sp.desc}`);
    for (const init of inits)
      if (init.address <= at && at < init.address + init.bytes.length) {
        const view = new DataView(init.bytes.buffer, init.bytes.byteOffset, init.bytes.byteLength);
        const off = at - init.address;
        if (off + 32 <= init.bytes.length)
          desc = Array.from({ length: 8 }, (_, i) => view.getUint32(off + 4 * i, true));
      }
    if (desc.length !== 8 || desc[0] !== 0x444b4d41)
      throw new ConformError(`${sp.module}: ${sp.desc} is no dk_machine_desc`);
    stride = (desc[4]! + 7) & ~7;
    clear = desc[5]!;
    addr = (addr + 7) & ~7;
    state = addr;
    inits.push({ address: addr, bytes: new Uint8Array(desc[3]! * stride), bss: true });
    addr += desc[3]! * stride;
  }
  if (addr > DATA_END)
    throw new ConformError(
      `${sp.module}: data symbols need 0x${(addr - DATA_BASE).toString(16)} bytes of L2, 0x${(DATA_END - DATA_BASE).toString(16)} are free`,
    );
  const { bases, end } = placeCode(objs);
  if (end > STOCK_BP)
    throw new ConformError(
      `${sp.module}: code ends at 0x${end.toString(16)}, past the stand-ins at 0x${STOCK_BP.toString(16)}`,
    );
  const { linked, symbols } = linkAll(objs, bases, { ...externsFixed(sp.device), ...data });
  return {
    linked,
    data,
    inits,
    symbols,
    state,
    stride,
    clear,
    desc,
    moduleCode: objs.slice(nwrap).map((o, i) => [bases[nwrap + i]!, bases[nwrap + i]! + o.parcels]),
    moduleData: layout.own.slice(nwrap).flat(),
    wrapperData: layout.own.slice(0, nwrap).flat(),
  };
}

export const FX_AUTO_STATES = 4; // a Row with `state = ""` gets `_fx_auto_state<row>`, this many bytes of poisoned bss each
export const FX_AUTO_STATE_BYTES = 4096;
export const FX_MASTER = 0x2009e000; // byte address of the 64 master floats
export const FX_RETURN = 0x2009e400; // the FX return bus: 64 floats
export const FX_TRACKBUS = 0x2009f000; // the 16 track buses of 64 floats, 0x100 bytes apart
export const FX_SLOTMAP = 0x2009e800; // DN2: the slot-to-track map, 16 halfwords in 8 words
export const FX_FADETAB = FX_SLOTMAP + 0x20; // DN2: the slot fade flags

export interface FxBuilt {
  linked: Linked[];
  data: Record<string, number>;
  inits: Init[];
  symbols: Record<string, number>;
  /** (first pc, end pc) of each module object (not the adapter or the send hook), short words */
  moduleCode: [number, number][];
  moduleData: Range[];
}

const fxBuilt = new Map<string, FxBuilt>();

/** Assemble, place and link the adapter, the send hook of the device and the effect sources (cached). */
export function buildFx(sources: readonly string[], device: 'dt2' | 'dn2' = 'dt2'): FxBuilt {
  const key = JSON.stringify([device, sources]);
  let b = fxBuilt.get(key);
  if (!b) fxBuilt.set(key, (b = buildFxUncached(sources, device)));
  return b;
}

/** A DSP source of an effect: its module's directories and the SDK's, or the SDK's alone for a source outside a module. */
function fxObject(file: string): SharcObject {
  const mod = moduleAround(file);
  const sdk = [path.join(ROOT, 'sdk/dsp/contract'), path.join(ROOT, 'sdk')];
  return assembleSource(file, mod ? includeDirs(mod) : sdk);
}

function buildFxUncached(sources: readonly string[], device: 'dt2' | 'dn2'): FxBuilt {
  const objs = [fxObject(FX_ADAPTER), fxObject(FX_SEND[device]!), ...sources.map(fxObject)];
  const layout = placeData(objs, DATA_BASE);
  let { end: addr } = layout;
  const { data, inits } = layout;
  const own = layout.own;
  for (let i = 0; i < FX_AUTO_STATES; i++) {
    data[`_fx_auto_state${i}`] = addr;
    inits.push({ address: addr, bytes: new Uint8Array(FX_AUTO_STATE_BYTES), bss: true });
    own.at(-1)!.push({ address: addr, bytes: FX_AUTO_STATE_BYTES });
    addr += FX_AUTO_STATE_BYTES;
  }
  // the module step word (`DK_STEP`, sdk/dsp/contract/dk_trace.inc); core/dyndsp exports it on the device
  data._dk_step = addr;
  inits.push({ address: addr, bytes: new Uint8Array(4), bss: true });
  own.at(-1)!.push({ address: addr, bytes: 4 });
  addr += 4;
  if (addr > DATA_END) throw new ConformError(`data symbols need 0x${(addr - DATA_BASE).toString(16)} bytes of L2`);
  const externs: Record<string, number> = {
    _fx_table: TABLE,
    _fx_master: FX_MASTER,
    _fx_back: STOCK_BP,
    _fxs_back: STOCK_BP, // one entry runs in a call: both tails end at the stand-in
    _fxs_trackbus: FX_TRACKBUS,
    _fxs_return: FX_RETURN,
    _fxs_slotbus: FX_TRACKBUS,
    _fxs_slotmap: FX_SLOTMAP,
    _fxs_fadetab: FX_FADETAB,
    ...data,
  };
  const { bases, end } = placeCode(objs);
  if (end > STOCK_BP) throw new ConformError(`code ends at 0x${end.toString(16)}, past the stand-ins`);
  const { linked, symbols } = linkAll(objs, bases, externs);
  return {
    linked,
    data,
    inits,
    symbols,
    moduleCode: objs.slice(2).map((o, i) => [bases[2 + i]!, bases[2 + i]! + o.parcels]),
    moduleData: own.slice(2).flat(),
  };
}
