import { asciiOf, be32, type DkLogEvent, type DkSession } from '@digi/dk-session';
import { CMD, LOG_KIND } from '@digi/dk-session/gen_proto';
import { button, MS, type Session } from '@digi/test-support';
import type { PatchedSession } from '../support.ts';
import { advance, rpc, word } from './fx-live.ts';
import { withPausedFxProtocol } from './fx-session.ts';
import { waitEmulatedFrames } from './waits.ts';
import { openDigikitMenu, readAboutRows } from './runtime-menu.ts';
import type { RuntimePlan } from './runtime-plan.ts';

/**
 * Helpers only the debug, card, crash and safe-mode scenarios use (tests/regression/runtime/{debug-module,card-safety,safe-mode,crash-capture,debug-guard}).
 * The card fixture is card-rig.ts.
 */

/** A time out of the 32-bit words of an event or a slot: no value. */
export const NONE = 0xffffffff;
export const CARD_TIMEOUT = 1_200_000;

/** Store commands that a card without a usable store refuses; the bodies are never valid uploads. */
export const OPS = {
  begin: [CMD.BEGIN, Buffer.concat([Buffer.from('modules/x.dkm\0'), be32(10), Buffer.alloc(32)])],
  delete: [CMD.DELETE, Buffer.from('modules/x.dkm\0')],
  mkdir: [CMD.MKDIR, Buffer.from('samples\0')],
  clear: [CMD.CLEAR, be32(0x434c5221)],
  list: [CMD.LIST, be32(0)],
} as const;

/** The status of each named store command. */
export async function opStatuses(dk: DkSession, names: (keyof typeof OPS)[]): Promise<number[]> {
  const out: number[] = [];
  for (const name of names) out.push((await dk.request(OPS[name][0], OPS[name][1])).status);
  return out;
}

// ---- the log ------------------------------------------------------------------------------------------------------------------------

/** The events of a CPU crash as the boot after it logged them (Python `dklog.crash_record`); null without a CRASH_FOUND. */
export function crashRecord(events: DkLogEvent[]) {
  const at = events.map((e) => e.kind === LOG_KIND.CRASH_FOUND).lastIndexOf(true);
  if (at < 0) return null;
  const first = events[at]!;
  const parts = new Set<number>([
    LOG_KIND.CRASH_MOD,
    LOG_KIND.CRASH_REG,
    LOG_KIND.CRASH_STACK,
    LOG_KIND.CRASH_TAIL,
    LOG_KIND.DSP_DIAG,
  ]);
  const rest: DkLogEvent[] = [];
  for (const e of events.slice(at + 1)) {
    if (e.kind === LOG_KIND.CRASH_FOUND) break;
    if (parts.has(e.kind)) rest.push(e); // the small ring's events (FAULT_FOUND, SETTINGS_WRITE) lie between the record's parts
  }
  const kind = (k: number) => rest.filter((e) => e.kind === k);
  const mod = kind(LOG_KIND.CRASH_MOD).find((e) => e.args[0] !== 0x100);
  return {
    corrupt: first.args[4] === 0xbad,
    pc: first.args[0]!,
    vector: first.args[1]! & 0xff,
    a7: first.args[3]!,
    regs: kind(LOG_KIND.CRASH_REG).flatMap((e) => e.args.slice(1).filter((_, i) => e.args[0]! + i < 16)),
    stack: kind(LOG_KIND.CRASH_STACK).flatMap((e) => e.args.slice(1)),
    /** Pairs of CRASH_TAIL events make one event of the boot before. */
    tail: Math.floor(kind(LOG_KIND.CRASH_TAIL).length / 2),
    module: mod ? { index: mod.args[0] === NONE ? null : mod.args[0]!, name: asciiOf(...mod.args.slice(2, 5)) } : null,
  };
}

// ---- emulated time and keys --------------------------------------------------------------------------------------------------------

/** Wait until the DSP has run `blocks` more frames (emulated time, not wall time). */
export function waitFrames({ session }: { session: Session }, blocks: number): Promise<void> {
  return waitEmulatedFrames(session, blocks);
}

/** The ABOUT page of the DIGIKIT menu, opened through the panel in a session that runs freely (keys held in emulated time). */
export async function openAbout(runtime: { session: Session }, plan: RuntimePlan): Promise<string[]> {
  const tap = async (name: string, settleMs = 1200) => {
    await runtime.session.client.inputTap(button(name), 100 * MS);
    await waitFrames(runtime, Math.round(settleMs * 1.5));
  };
  await openDigikitMenu(tap);
  await tap('YES', 3000); // ABOUT is the first row
  return readAboutRows(runtime.session, plan);
}

/** Pause the run and give `op` a protocol session that steps the emulator (the scenarios that press keys with stepped time). */
export async function pausedProtocol<T>(runtime: PatchedSession, op: (dk: DkSession) => Promise<T>): Promise<T> {
  await rpc(runtime.session, 'run.mode', { mode: 'paused' });
  return withPausedFxProtocol(runtime, op);
}

/** Run in steps (the run must be paused) until the word at `address` stops changing; false when it never does within `limitMs`. */
export async function settleHang(session: Session, address: number, limitMs = 9000): Promise<boolean> {
  for (let i = 0; i < Math.floor(limitMs / 400); i++) {
    const before = await word(session, address);
    await advance(session, 400);
    if ((await word(session, address)) === before) return true;
  }
  return false;
}
