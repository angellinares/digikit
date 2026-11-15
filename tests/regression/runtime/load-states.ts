import { LOAD_STATE, LOAD_STATUS } from '@digi/dk-session/gen_proto';

/** The name of a module state number (`[load.state]`, `DkrtBootInfo.states`). */
export const stateName = (n: number): string => LOAD_STATE.find(([code]) => code === n)?.[1] ?? `?${n}`;

/** The number of a load status name (`E_PLAN_FIT` is -78). */
export const loadCode = (name: string): number => {
  const hit = LOAD_STATUS.find(([, n]) => n === name);
  if (!hit) throw new Error(`no load status ${name}`);
  return hit[0];
};

/** The load codes only the boot plan gives a module of the selection: the module is REFUSED and none of its code runs. `E_CONTRACT` and `E_MISSING` are in it
 * too, but the loader gives them as well, so a test names the state it expects for those. */
export const PLAN_ONLY = ['E_PLAN_KIND', 'E_PLAN_OWNER', 'E_PLAN_KEY', 'E_PLAN_FIT'].map(loadCode);

/** The states a module may be in at the end of the boot's loads for a wanted result: `want` is the load code, or null for a module that loads (ACTIVE, or
 * DSP_LOADING while the DSP part is going). */
export function statesFor(want: number | null): string[] {
  if (want === null) return ['ACTIVE', 'DSP_LOADING'];
  if (PLAN_ONLY.includes(want)) return ['REFUSED'];
  if (want === loadCode('E_SAFE_MODE')) return ['SKIPPED'];
  return ['FAILED'];
}
