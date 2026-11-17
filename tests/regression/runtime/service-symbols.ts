import fs from 'node:fs';
import type { Session } from '@digi/test-support';
import { readLoadTable } from './addrmap.ts';
import { planSymbol, shipMapOf, type RuntimePlan } from './runtime-plan.ts';
import { boundMapPath, SERVICE_MODULES } from './service-files.ts';

/** What a lookup needs of a runtime: its session (`PatchedSession`, `TestRuntime` and `Rt` all have it). */
type WithSession = { readonly session: Session };

/** How a lookup finds the service: the ship map of the image, which the load table is decoded with. */
export interface LookupOptions {
  shipMap?: string;
}

/**
 * The load address of service `service`: its record in the device load table (`readLoadTable`, docs/ADDRMAP.md), found by the module id of the service file. The table
 * is read from the running device's memory, so it works while the daemon is paused. The test that asks is a live-patchkit task (it decodes the table with `patchkit addr`).
 */
async function loadAddress(
  runtime: WithSession,
  plan: RuntimePlan,
  service: string,
  opts: LookupOptions,
): Promise<number> {
  const id = SERVICE_MODULES[service];
  if (!id) throw new Error(`unknown service ${service}`);
  const shipMap = opts.shipMap ?? shipMapOf(plan);
  if (!shipMap) throw new Error(`no address map for the image of this plan: prepare the plan with runtime-plan.ts`);
  const hit = (await readLoadTable(runtime.session, plan, shipMap)).find((l) => l.id === id);
  if (!hit)
    throw new Error(
      `service ${service} (${id}) is not in the load table: the card has no ${service}.dks, or it was refused`,
    );
  return hit.base;
}

/**
 * The symbols of the service as bound to the stock firmware (docs/ADDRMAP.md): name to offset from the load address. The bound map is the one `patchkit bind` writes
 * (`dist/svc-bound/<device>/<name>.addrmap.json`): bind appends tails and trampolines and moves `.bss`, so the map of the unbound pack is wrong for those symbols.
 */
export function serviceMapEntries(device: Session['options']['device'], service: string): Map<string, number> {
  const file = boundMapPath(device, service);
  if (!fs.existsSync(file)) throw new Error(`${file}: run nx run ${SERVICE_MODULES[service]}:bind`);
  const map = JSON.parse(fs.readFileSync(file, 'utf8')) as { entries: { cpu: string; addr: number; name: string }[] };
  return new Map(map.entries.filter((e) => e.cpu === 'cf').map((e) => [e.name, e.addr]));
}

/**
 * The address of `symbol` of a loaded service, e.g. `serviceSymbol(runtime, plan, 'fx', 'DKRT_FXP')`: the load address plus the offset in the service's address map.
 */
export async function serviceSymbol(
  runtime: WithSession,
  plan: RuntimePlan,
  service: string,
  symbol: string,
  opts: LookupOptions = {},
): Promise<number> {
  const entries = serviceMapEntries(runtime.session.options.device, service);
  const offset = entries.get(symbol);
  if (offset === undefined) throw new Error(`${symbol} is not in the address map of the ${service} service`);
  return (await loadAddress(runtime, plan, service, opts)) + offset;
}

/**
 * A name lookup for a test that reads the state of a service and of the image together: the names of the service's own symbols (`DKRT_FXP`, `DKRT_FXS`, the
 * `DKRT_FXP_*` statics; the old lower-case `fxp_*` names of the hook labels map to them) resolve through the service, every other name through the image's plan.
 * The base is read once: call it again after a reload that can move the service.
 */
export async function serviceSymbols(
  runtime: WithSession,
  plan: RuntimePlan,
  service: string,
  opts: LookupOptions = {},
): Promise<(name: string) => number> {
  const entries = serviceMapEntries(runtime.session.options.device, service);
  const base = await loadAddress(runtime, plan, service, opts);
  return (name) => {
    const own = entries.has(name) ? name : name.startsWith('fxp_') ? `DKRT_${name.toUpperCase()}` : name;
    const offset = entries.get(own);
    return offset === undefined ? planSymbol(plan, name) : base + offset;
  };
}

const bound = new WeakMap<object, (name: string) => number>();

/** Bind the symbols of `service` for the session after a boot (`inspectFxBoot`). A fixture that declares `noServices` has no service: pass `expected: false` and the
 * symbols are cleared. Otherwise a service that cannot be found is an error naming the service and the reason (no silent fallback to the image's plan). */
export async function bindServiceSymbols(
  runtime: WithSession,
  plan: RuntimePlan,
  service: string,
  opts: LookupOptions & { expected?: boolean } = {},
): Promise<void> {
  if (opts.expected === false) {
    bound.delete(runtime.session);
    return;
  }
  try {
    bound.set(runtime.session, await serviceSymbols(runtime, plan, service, opts));
  } catch (e) {
    bound.delete(runtime.session);
    throw new Error(
      `symbols of the ${service} service are not available: ${e instanceof Error ? e.message : String(e)}`,
    );
  }
}

/** True when the last boot of the session had the service (`bindServiceSymbols` bound its symbols): false for a boot without the service file, which has none of its cells. */
export function hasServiceSymbols(runtime: WithSession): boolean {
  return bound.has(runtime.session);
}

/** The name lookup of the last boot of the session (`bindServiceSymbols`), or the image's plan alone when no service was found. Symbols of the fx service: `DKRT_FXP`,
 * `DKRT_FXS`, `DKRT_FXP_TAB`, `DKRT_FXP_COUNT`, `DKRT_FXP_SEND_ROW`, ... (the old `fxp_*` names map to them). */
export function symbolsOf(runtime: WithSession, plan: RuntimePlan): (name: string) => number {
  return bound.get(runtime.session) ?? ((name) => planSymbol(plan, name));
}
