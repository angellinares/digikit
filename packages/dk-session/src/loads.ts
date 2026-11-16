// The LOADS reply in words: the state of each module of the boot selection and why a module is not active (docs/plans/runtime-and-services.md section 8,
// item R11). The numbers are `[load.state]` and `[load.status]` of sdk/sdk.toml (gen_proto.ts). Written without TS-only runtime syntax so node can import it.
import { CAPACITY_LABELS, LOAD_STATE, LOAD_STATUS } from './gen_proto.ts';

export type ModuleStateName =
  'SELECTED' | 'PLANNED' | 'REFUSED' | 'INIT' | 'DSP_LOADING' | 'ACTIVE' | 'FAILED' | 'SKIPPED';

/** A module of the last boot's selection as the runtime's LOADS record has it. */
export interface ModuleState {
  /** Position in the selection. */
  index: number;
  /** The file name the runtime kept (`usb-fast.dkm`, 27 characters at most). */
  name: string;
  state: ModuleStateName;
  /** OK (0), LOADING (1) or the negative load code that says why. */
  result: number;
  /** What the boot plan refused on (version 2; zeros for a version 1 reply): by code, E_PLAN_KIND the kind, E_MISSING the name hash of the service, E_PLAN_OWNER the kind, slot or hook site,
   * E_PLAN_KEY the kind and the key, E_PLAN_FIT the capacity kind. */
  detail: [number, number];
  /** E_IMPORT only: the module its imports need that is not in the image, without the `core/` prefix (usb-bulk). */
  needs?: string;
}

const E_IMPORT = -7;
const E_SAFE_MODE = -44;
const E_CONTRACT = -48;
const E_MISSING = -73;
const E_PLAN_KIND = -75;
const E_PLAN_OWNER = -76;
const E_PLAN_KEY = -77;
const E_PLAN_FIT = -78;
const PLAN_CODES: number[] = [E_PLAN_KIND, E_PLAN_OWNER, E_PLAN_KEY, E_PLAN_FIT];

/** Bytes of one module in a version 1 reply (result, needs, name) and in a version 2 reply (result, state, 3 zero bytes, detail, needs, name). */
const ENTRY_V1 = 44;
const ENTRY_V2 = 56;

/** The state a version 1 reply implies: it carried no state, so it follows from the result code (the plan's codes are a refusal; E_CONTRACT and E_MISSING
 * cannot be told apart from a loader failure and count as failures). */
function impliedState(result: number): ModuleStateName {
  if (result === 0) return 'ACTIVE';
  if (result === 1) return 'DSP_LOADING';
  if (result === E_SAFE_MODE) return 'SKIPPED';
  return PLAN_CODES.includes(result) ? 'REFUSED' : 'FAILED';
}

/** Read the body of a LOADS reply (version 1 or 2). */
export function parseLoads(b: Uint8Array): ModuleState[] {
  const v = new DataView(b.buffer, b.byteOffset, b.byteLength);
  const version = b[0];
  if (version !== 1 && version !== 2) throw new Error(`LOADS: unknown version ${version}`);
  const entry = version === 2 ? ENTRY_V2 : ENTRY_V1;
  const n = b[1]!;
  if (b.length < 2 + entry * n) throw new Error('LOADS: short reply');
  const str = (at: number, len: number) => {
    let out = '';
    for (let i = 0; i < len && b[at + i]; i++) out += String.fromCharCode(b[at + i]!);
    return out;
  };
  const out: ModuleState[] = [];
  for (let i = 0; i < n; i++) {
    const at = 2 + entry * i;
    const result = v.getInt32(at);
    let state: ModuleStateName;
    let detail: [number, number] = [0, 0];
    let rest = at + 8;
    if (version === 2) {
      const named = LOAD_STATE.find(([code]) => code === b[at + 4])?.[1] as ModuleStateName | undefined;
      if (!named) throw new Error(`LOADS: unknown state ${b[at + 4]}`);
      state = named;
      detail = [v.getUint32(at + 8), v.getUint32(at + 12)];
      rest = at + 16;
    } else {
      state = impliedState(result);
      rest = at + 4;
    }
    const needs = str(rest, 12);
    out.push({ index: i, name: str(rest + 12, 28), state, result, detail, ...(needs ? { needs } : {}) });
  }
  return out;
}

/** The module name without its `.dkm`: `usb-fast.dkm` is `usb-fast`. */
export const stateModule = (m: Pick<ModuleState, 'name'>): string => m.name.replace(/\.dkm$/, '');

const hex = (n: number): string => `0x${n.toString(16)}`;

/** Why a module is not active, in words about the module; undefined for ACTIVE. The one place of these texts: Transfer shows it, the menu's short forms follow it. */
export function moduleReason(m: ModuleState): string | undefined {
  if (m.state === 'ACTIVE') return undefined;
  if (m.state === 'DSP_LOADING') return 'its DSP part is still loading';
  if (m.state === 'SELECTED' || m.state === 'PLANNED' || m.state === 'INIT') return 'loading has not finished';
  if (m.state === 'SKIPPED') return 'this boot is in safe mode, so no module is loaded';
  if (m.state === 'REFUSED') {
    if (m.result === E_PLAN_FIT) {
      const what = CAPACITY_LABELS[m.detail[0]] ?? `resource ${m.detail[0]}`;
      return `no room: the ${what} that the earlier modules of the selection left is not enough`;
    }
    if (m.result === E_PLAN_KIND)
      return `its records of kind ${m.detail[0]} have no owner: no service or selected module takes that kind`;
    if (m.result === E_PLAN_OWNER)
      return `another module already owns ${hex(m.detail[0])} (a record kind, slot or hook site it takes)`;
    if (m.result === E_PLAN_KEY)
      return `another selected module has the same key ${hex(m.detail[1])} for record kind ${m.detail[0]}`;
    if (m.result === E_MISSING)
      return m.detail[0]
        ? `the service ${hex(m.detail[0])} that it needs is not provided by the runtime, and no provider file for it loaded`
        : 'a service it needs is not provided by the runtime or by an earlier selected module';
    if (m.result === E_CONTRACT) return 'it needs a newer runtime than this one';
  }
  if (m.result === E_IMPORT && m.needs)
    return `this firmware has no core/${m.needs}. Flash firmware from the Firmware page.`;
  return LOAD_STATUS.find(([code]) => code === m.result)?.[2] ?? `error ${m.result}`;
}

/** One line for the Transfer list and the log: `pluck refused: no room: ...`, `usb-fast not loaded: ...`. Undefined for a module that is active or loading. */
export function moduleStateText(m: ModuleState): string | undefined {
  const why = moduleReason(m);
  if (why === undefined || m.state === 'DSP_LOADING') return undefined;
  const lead = m.state === 'REFUSED' ? 'refused' : m.state === 'SKIPPED' ? 'skipped' : 'not loaded';
  return `${stateModule(m)} ${lead}: ${why}`;
}
