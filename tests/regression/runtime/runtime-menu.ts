import {
  asciiOf,
  declDiagnosis,
  fmtVersion,
  hasCap,
  PROVIDER_IDX,
  type DkLogEvent,
  type DkSession,
} from '@digi/dk-session';
import { LOAD_STATUS, LOG_KIND } from '@digi/dk-session/gen_proto';
import { emulatorClock, pollUntil, type Session } from '@digi/test-support';
import type { PatchedSession } from '../support.ts';
import { record } from './artifacts.ts';
import { planSymbol, type RuntimePlan } from './runtime-plan.ts';

/** fill_about publishes incrementally: the runtime row alone precedes BOOT and the final RAM row. */
export const aboutIsComplete = (rows: string[]): boolean =>
  rows.some((row) => /^BOOT: \d+ OF \d+ OK$/.test(row)) && /^RAM KEPT: (YES|NO)$/.test(rows.at(-1) ?? '');

export async function readAboutRows(session: Session, plan: RuntimePlan): Promise<string[]> {
  const base = planSymbol(plan, 'DK_ABOUT');
  const peek = (address: number) => session.client.memPeek(address);
  const count = await peek(planSymbol(plan, 'DK_ABOUT_N'));
  if (count === null || !Number.isInteger(count) || count < 1 || count > 24)
    throw new Error(`invalid ABOUT row count: ${count}`);
  const rows: string[] = [];
  for (let i = 0; i < count; i++) {
    const row = Buffer.alloc(24);
    for (let offset = 0; offset < 24; offset += 4) {
      const word = await peek(base + i * 24 + offset);
      if (word === null) throw new Error('ABOUT row is unmapped');
      row.writeUInt32BE(word >>> 0, offset);
    }
    rows.push(row.toString('latin1').split('\0')[0]!);
  }
  return rows;
}

/** Poll semantic menu content, bounded in emulated time. */
export async function expectRuntimeAbout(
  session: Session,
  dk: DkSession,
  plan: RuntimePlan,
  artifacts: string,
  options: { timeout?: number } = {},
): Promise<string[]> {
  let runtimeRow: string | undefined;
  let rows: string[] = [];
  let failed = true;
  try {
    runtimeRow = `RUNTIME ${fmtVersion((await dk.info()).runtime)}`;
    rows = await pollUntil(
      emulatorClock(session),
      async () => (rows = await readAboutRows(session, plan)),
      (r) => r.includes(runtimeRow!) && aboutIsComplete(r),
      {
        withinMs: options.timeout ?? 10_000,
        message: 'ABOUT completely describes the live runtime',
        intervalMs: 500,
        retry: true,
      },
    );
    failed = false;
    return rows;
  } finally {
    record(artifacts, 'about-readiness', { runtimeRow: runtimeRow ?? null, rows, expired: failed });
  }
}

/** Stable identity/boot semantics only; other ABOUT rows may contain changing diagnostics. */
export function meaningfulAboutRows(rows: string[]): string[] {
  return rows.filter((row) => /^(DIGIKIT |RUNTIME |BOOT: )/.test(row));
}

/** The symbols `readSmallRing` and `readCrashSeen` need in the runtime plan. */
export const RING_SYMBOLS = ['DKRT_LOG', 'DKRT_LOG_COUNT', 'DKRT_CRASH'];

const RING_EVENTS = 32;

async function word(session: Session, address: number): Promise<number> {
  const value = await session.client.memPeek(address);
  if (value === null) throw new Error(`guest memory at ${address.toString(16)} is unmapped`);
  return value >>> 0;
}

/**
 * The runtime's own ring of boot and error events (`dlog.rs`, 32 events), read from guest memory. Safe mode loads no module, so the debug module's LOG is
 * not there after a stop or a crash; this ring is what the runtime itself keeps, and what a crash record takes its tail from.
 */
export async function readSmallRing(session: Session, plan: RuntimePlan): Promise<DkLogEvent[]> {
  const base = planSymbol(plan, 'DKRT_LOG');
  const count = await word(session, planSymbol(plan, 'DKRT_LOG_COUNT'));
  const events: DkLogEvent[] = [];
  for (let seq = Math.max(1, count - RING_EVENTS + 1); seq <= count; seq++) {
    const at = base + ((seq - 1) % RING_EVENTS) * 32;
    const w = await Promise.all([0, 1, 2, 3, 4, 5, 6, 7].map((k) => word(session, at + 4 * k)));
    if (w[0] !== seq) continue;
    events.push({ seq, time: w[1]!, kind: w[2]! >>> 16, flags: w[2]! & 0xffff, args: w.slice(3, 8) });
  }
  return events;
}

export const ofKind = (events: DkLogEvent[], kind: number) => events.filter((e) => e.kind === kind);

/** The events of one kind in the runtime's small ring (what exists while the debug module is not loaded). */
export async function ringEvents(
  session: Session,
  plan: RuntimePlan,
  kind: keyof typeof LOG_KIND,
): Promise<DkLogEvent[]> {
  return ofKind(await readSmallRing(session, plan), LOG_KIND[kind]);
}

/** Like `ringEvents`, without the events of providers (`services/*.dks`, which log with `PROVIDER_IDX` as their index): the events of modules only. */
export async function moduleRingEvents(
  session: Session,
  plan: RuntimePlan,
  kind: keyof typeof LOG_KIND,
): Promise<DkLogEvent[]> {
  return (await ringEvents(session, plan, kind)).filter((e) => e.args[0] !== PROVIDER_IDX);
}

/** Every event the device's log holds: over SysEx while the debug module is loaded, else the runtime's own small ring from memory. */
export async function logEvents({ session, dk }: PatchedSession, plan: RuntimePlan): Promise<DkLogEvent[]> {
  return hasCap(await dk.info(), 'LOG') ? dk.readLog() : readSmallRing(session, plan);
}

const E_MISSING = -73;
/** `PROVIDER_IDX` of loader/mod.rs: the index of a MOD_LOAD event of a provider file (`services/<name>.dks`), which is no selection index. */
const PROVIDER_INDEX = PROVIDER_IDX;

/**
 * One provider MOD_LOAD event as text: `provider fx.d res -7 E_IMPORT`; the name is the first four characters of the file name. An E_DECL_KIND refusal names the first
 * item that `init` and the header table do not both hold: the event's service hash word is the item's first word and its base word the tag, plus 0x100 when only
 * the header table has the item (loader/mod.rs `decl_diff`): `provider mach res -79 E_DECL_KIND (EXPORTS 946ae751 not declared)`.
 */
export function providerFailureLine(args: readonly number[]): string {
  const res = args[1]! | 0;
  let line = `provider ${asciiOf(args[2]!)} res ${res} ${LOAD_STATUS.find(([code]) => code === res)?.[1] ?? '?'}`;
  const why = declDiagnosis(args);
  return why ? `${line} (${why})` : line;
}

/**
 * The load results of the provider files, from the error events of the runtime's small ring (the debug module is not needed), joined by `; `, each as
 * `providerFailureLine` gives it. Empty when no provider failed. A module refused with E_MISSING (-73)
 * only says that its provider did not load: this says why.
 */
export async function providerFailures(session: Session, plan: RuntimePlan): Promise<string> {
  const lines = ofKind(await readSmallRing(session, plan), LOG_KIND.MOD_LOAD)
    .filter((e) => e.args[0] === PROVIDER_INDEX && (e.args[1]! | 0) < 0)
    .map((e) => providerFailureLine(e.args));
  return lines.join('; ');
}

/**
 * The text to put after a failed wait on module results: the provider results when a module result is E_MISSING, else nothing. For the failure message only.
 */
export async function moduleResultsNote(
  session: Session,
  plan: RuntimePlan,
  results: readonly number[],
): Promise<string> {
  if (!results.some((r) => (r | 0) === E_MISSING)) return '';
  try {
    const why = await providerFailures(session, plan);
    return why ? ` [${why}]` : ' [no provider failure in the runtime ring]';
  } catch {
    return '';
  }
}

/** What the boot found of a crash record (`DKRT_CRASH`, fault.rs `CrashSeen`): the words that tests look at. */
export async function readCrashSeen(
  session: Session,
  plan: RuntimePlan,
): Promise<{ found: number; panic: number; tail: number }> {
  const base = planSymbol(plan, 'DKRT_CRASH');
  return {
    found: await word(session, base),
    panic: await word(session, base + 40),
    tail: await word(session, base + 44),
  };
}
