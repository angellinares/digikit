import fs from 'node:fs';
import path from 'node:path';
import type { DkModule } from '@digi/dk-session';
import { repoRoot, type Device, type Session } from '@digi/test-support';
import { parse } from 'smol-toml';
import { freeRegions } from '../support.ts';
import { openDigikitMenu } from './runtime-menu.ts';
import { advance, word } from './fx-live.ts';
import { tap } from './fx-session.ts';
import {
  buildPanelProject,
  loadPanelProject,
  savePanelProject,
  sweepMachineList,
  type PanelMachine,
} from './project-kit.ts';
import { planSymbol, type RuntimePlan } from './runtime-plan.ts';

/**
 * The ColdFire memory audit (port of tools/patchkit/pk_digikit_memaudit.py).
 *
 * Protected: the ColdFire free regions of the port file minus the block of every loaded module (`cf_base` .. `cf_base + cf_size` of the MODULES
 * reply); the part of the arena outside the module blocks is labelled `arena`. Allowed writers (`allow_pc`): the code the patchkit placed in the
 * image, and the runtime's object up to the start of the arena. Not covered: stores by DMA and by the host, and a store the runtime makes for a
 * module (an API call: the writer is the runtime).
 */

export interface Span {
  lo: number;
  hi: number;
}
export interface Range extends Span {
  label: string;
}
export interface Owned extends Span {
  owner: string;
}

/** `ranges` without [lo, hi); with `label` the cut-out part stays under that label (the pieces of a range keep its name). */
export function cutRanges(ranges: Range[], lo: number, hi: number, label?: string): Range[] {
  const out: Range[] = [];
  for (const r of ranges) {
    if (hi <= r.lo || lo >= r.hi) {
      out.push(r);
      continue;
    }
    if (r.lo < lo) out.push({ label: r.label, lo: r.lo, hi: lo });
    if (label !== undefined) out.push({ label, lo: Math.max(r.lo, lo), hi: Math.min(r.hi, hi) });
    if (hi < r.hi) out.push({ label: r.label, lo: hi, hi: r.hi });
  }
  return out;
}

/** The code that may write anywhere it needs: every object the patchkit placed and the hook tails; of the object that holds the arena only the part below it. */
export function patchCode(plan: RuntimePlan, arenaLo: number): Owned[] {
  if (!plan.writes) throw new Error('the runtime plan lists no writes');
  const out: Owned[] = [];
  for (const w of plan.writes) {
    if (w.cpu !== 'cf' || w.kind !== 'place') continue;
    if (!(w.what.startsWith('object in') || w.what.startsWith('hook tail'))) continue;
    let hi = w.addr + w.len;
    if (w.addr < arenaLo && arenaLo < hi) hi = arenaLo;
    out.push({ owner: w.owner.split('@')[0]!, lo: w.addr, hi });
  }
  return out;
}

export interface GuardPlan {
  protect: Range[];
  /** The allowed writers as owned spans, for naming a hit. */
  code: Owned[];
}

/** The protected ranges and the allowed code for the loaded modules and the arena bounds. */
export function guardPlan(
  device: Device,
  firmware: string,
  plan: RuntimePlan,
  modules: Pick<DkModule, 'cf_base' | 'cf_size'>[],
  arena: [number, number],
): GuardPlan {
  let protect: Range[] = freeRegions(device, firmware).map((r) => ({ label: r.name, lo: r.lo, hi: r.hi }));
  protect = cutRanges(protect, arena[0], arena[1], 'arena');
  for (const m of modules) protect = cutRanges(protect, m.cf_base, m.cf_base + m.cf_size);
  return { protect: protect.filter((r) => r.hi > r.lo), code: patchCode(plan, arena[0]) };
}

/** The code a PC belongs to: a module's block, a patch object (the runtime included), else the stock firmware. */
export function whoWrote(pc: number, modules: Pick<DkModule, 'name' | 'cf_base' | 'cf_size'>[], code: Owned[]): string {
  for (const m of modules) if (m.cf_base <= pc && pc < m.cf_base + m.cf_size) return `module ${m.name}`;
  for (const c of code) if (c.lo <= pc && pc < c.hi) return `patch ${c.owner}`;
  return 'stock';
}

/** The arena bounds the runtime keeps in `DKRT_STATE` (words 7 and 8). */
export async function arenaBounds(session: Session, plan: RuntimePlan): Promise<[number, number]> {
  const state = planSymbol(plan, 'DKRT_STATE');
  return [await word(session, state + 28), await word(session, state + 32)];
}

/** The current bytes of every module's block, read from guest memory. */
export async function moduleBlocks(session: Session, modules: DkModule[]): Promise<Map<string, Buffer>> {
  const out = new Map<string, Buffer>();
  for (const m of modules) {
    const bytes = Buffer.alloc((m.cf_size + 3) & ~3);
    for (let i = 0; i < bytes.length; i += 4) bytes.writeUInt32BE(await word(session, m.cf_base + i), i);
    out.set(m.name, bytes.subarray(0, m.cf_size));
  }
  return out;
}

/** Bytes that differ between two reads of the same block. */
export const bytesChanged = (a: Buffer, b: Buffer) => a.reduce((n, x, i) => n + (x !== b[i] ? 1 : 0), 0);

export interface AuditHit {
  pc: number;
  addr: number;
  size: number;
  label: string;
  writer: string;
}

/** The guard's report with each hit attributed to its writer. */
export async function guardHits(
  session: Session,
  modules: DkModule[],
  code: Owned[],
): Promise<{ total: number; hits: AuditHit[] }> {
  const report = await session.client.guardReport();
  return {
    total: report.total,
    hits: report.hits.map((h) => ({
      pc: h.pc,
      addr: h.addr,
      size: h.size,
      label: h.label,
      writer: whoWrote(h.pc, modules, code),
    })),
  };
}

const hex = (n: number) => `0x${n.toString(16)}`;
export const describeHits = (hits: AuditHit[]) =>
  hits.map((h) => `pc ${hex(h.pc)} (${h.writer}) addr ${hex(h.addr)} size ${h.size} in ${h.label}`).join('; ');

/** The `DIGI_WRITE_WATCH*` variables of the load pass: the protected ranges (CPU stores), the allowed code, from the first cache enable on (the log file stays the daemon's default). */
export function loadWatchEnv(guard: GuardPlan, holes: [number, number][]): Record<string, string> {
  let protect = guard.protect;
  for (const [start, size] of holes) protect = cutRanges(protect, start, start + size);
  return {
    DIGI_WRITE_WATCH: protect
      .filter((r) => r.hi > r.lo)
      .map((r) => `${hex(r.lo)}-${hex(r.hi)}`)
      .join(','),
    DIGI_WRITE_WATCH_ALLOW_PC: guard.code.map((c) => `${hex(c.lo)}-${hex(c.hi)}`).join(','),
    DIGI_WRITE_WATCH_AFTER: 'cacr',
  };
}

/** The UI for a bounded time: the DIGIKIT menu and its MODULES page (looked at, nothing ticked), the machines on their tracks with parameter
 * turns, a p-lock and a sound lock, one project save, an edit, a second save and a load. `sweep`: also choose each of the first entries of the machine list.
 * `health: false` when a guard hit is expected (the session counts it as a fault). */
export async function driveAuditUi(
  session: Session,
  machines: PanelMachine[],
  sweep = 0,
  health = true,
): Promise<void> {
  const press = (key: string, ms = 300) => tap(session, key, ms);
  await openDigikitMenu(press); // SETTINGS > DIGIKIT (the ninth row)
  for (let i = 0; i < 3; i++) await press('DOWN', 200);
  await press('YES', 900); // MODULES
  for (const key of ['DOWN', 'DOWN', 'UP', 'NO', 'NO', 'NO']) await press(key, 300);
  await savePanelProject(session);
  await buildPanelProject(session, machines);
  if (sweep) await sweepMachineList(session, sweep);
  await savePanelProject(session);
  await loadPanelProject(session, { health });
  await advance(session, 1000);
}

// ---- module sets for `mise run module:audit` ------------------------------------------------------------------------------------------

interface Manifest {
  module: { id: string; type?: string; requires?: string[]; ports?: string[] };
  targets?: Record<string, string>;
  machine?: unknown;
}

/** Module id -> manifest over `modules/`, `tests/fixtures/modules/` and the `PATCHKIT_MODULES` roots (the ids of module.toml files). */
export function moduleManifests(): Map<string, Manifest> {
  const root = repoRoot();
  const roots = [
    path.join(root, 'modules'),
    path.join(root, 'tests/fixtures/modules'),
    ...(process.env.PATCHKIT_MODULES ?? '').split(path.delimiter).filter(Boolean),
  ];
  const out = new Map<string, Manifest>();
  const walk = (dir: string) => {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      const full = path.join(dir, entry.name);
      if (entry.isDirectory()) {
        if (entry.name !== 'node_modules' && entry.name !== 'build' && entry.name !== 'src') walk(full);
      } else if (entry.name === 'module.toml') {
        const m = parse(fs.readFileSync(full, 'utf8')) as unknown as Manifest;
        out.set(m.module.id, m);
      }
    }
  };
  for (const r of roots) if (fs.existsSync(r)) walk(r);
  return out;
}

/** A module id from an id or its last part. */
export function resolveModule(name: string, manifests = moduleManifests()): string {
  if (manifests.has(name)) return name;
  const hits = [...manifests.keys()].filter((id) => id.split('/').at(-1) === name);
  if (hits.length !== 1)
    throw new Error(`module ${name}: ${hits.length ? `ambiguous (${hits.join(', ')})` : 'unknown'}`);
  return hits[0]!;
}

/** `id` and the run-time modules it requires, in load order (the `core/` modules are patched in). */
export function auditSet(id: string, manifests = moduleManifests()): string[] {
  const out: string[] = [];
  const add = (m: string) => {
    if (out.includes(m)) return;
    for (const r of manifests.get(m)?.module.requires ?? []) if (manifests.has(r) && !r.startsWith('core/')) add(r);
    out.push(m);
  };
  add(id);
  return out;
}

export const hasMachine = (id: string, manifests = moduleManifests()) => {
  const m = manifests.get(id);
  return m?.module.type === 'machine' || (m !== undefined && 'machine' in m);
};

/** Machine list entries chosen on the track in the sweep of `module:audit` (the stock list is shorter on both devices: the run-time machines follow it). */
export const AUDIT_SWEEP = 14;
