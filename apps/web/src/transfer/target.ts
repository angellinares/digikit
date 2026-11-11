// The target of the Transfer pane: the emulator (where the app has one), or a MIDI port with a real device on it. Every page that talks the
// digikit protocol (Transfer, Remote, the Memory panel of the workbench) uses the one selected here, through the one session of that target.
// The protocol client is the same in every case (`DkSession`); only the transport differs, and the app gives it (env.ts, `targets`).
import { createSignal } from 'solid-js';
import { DkSession } from '@digi/dk-session';
import type { DkTransport } from '@digi/dk-session';
import { NoRuntimeError } from '../modules/backend';
import { instrument } from './activity';
import { noteConnection } from './connection-state';
import { transferEnv } from './env';
import { createSessions, targetKey } from './session-core';

/** `none`: the site before any MIDI port is there (it has no emulator); a request on it fails with a message that says what to do. */
export type Target = { kind: 'emulator' } | { kind: 'midi'; port: string } | { kind: 'none' };

const KEY = 'digikit.target';
const read = (): Target => {
  try {
    const t = JSON.parse(localStorage.getItem(KEY) ?? 'null') as Target | null;
    if (t?.kind === 'midi' && typeof t.port === 'string' && t.port) return t;
  } catch {
    // no storage: the emulator
  }
  return { kind: 'emulator' };
};

export const [target, setTargetSignal] = createSignal<Target>(read());

/** The MIDI port chosen last, also while the Emulator is the target: the workbench's Log follows the device on it. Kept per profile. */
const MIDI_KEY = 'digikit.lastMidi';
const readMidi = (): string | undefined => {
  const t = target();
  if (t.kind === 'midi') return t.port;
  try {
    const port = localStorage.getItem(MIDI_KEY);
    if (port) return port;
  } catch {
    // no storage: none
  }
  return undefined;
};
export const [lastMidiPort, setLastMidiPort] = createSignal<string | undefined>(readMidi());

export const NO_PORT = 'No MIDI port';
export const targetLabel = (t: Target): string =>
  t.kind === 'emulator' ? 'Emulator' : t.kind === 'none' ? NO_PORT : `MIDI: ${t.port}`;
export const sameTarget = (a: Target, b: Target): boolean =>
  a.kind === b.kind && (a.kind !== 'midi' || a.port === (b as { port: string }).port);

/** Start on this target without opening it or keeping the choice (the site picks its first target when the ports are known). */
export function startOn(t: Target): void {
  if (sameTarget(t, target())) return;
  opened = undefined;
  setTargetSignal(t);
  noteConnection({ type: 'target' });
}

/** The digikit runtime of the emulator as the app knows it from the last boot (the desktop sets it; the site has no emulator): 'unknown' while booting. */
export const [emulatorRuntime, setEmulatorRuntime] = createSignal<'unknown' | 'yes' | 'no'>('unknown');
export const EMULATOR_NO_RUNTIME =
  'The emulator runs without the digikit runtime: tick Boot with runtime and reload it.';
/** Throws NoRuntimeError for a target known to have no runtime (an emulator booted without it), before any request. */
export function checkRuntime(t: Target): void {
  if (t.kind === 'emulator' && emulatorRuntime() === 'no') throw new NoRuntimeError(EMULATOR_NO_RUNTIME);
}

/** The transport of each target in use (made once and instrumented once, so a new session of the same port reuses it). */
const transports = new Map<string, DkTransport>();

/** One session per target (session-core.ts): the emulator's, and the open MIDI port's. Every page and panel uses these, never a session of its own. */
const sessions = createSessions<DkSession>(
  (key) => new DkSession(transports.get(key) as DkTransport, { timeoutMs: 6000 }),
);
let opened: string | undefined;

async function openTransport(t: Target): Promise<void> {
  transports.set(targetKey(t), instrument(await transferEnv().targets.open(t)));
}

function forget(port: string): void {
  const key = targetKey({ kind: 'midi', port });
  sessions.drop(key);
  transports.delete(key);
}

/** The transport of a MIDI port ended by itself (the device was unplugged): the next request opens the port again, and fails if it is not there. */
export function portLost(port: string): void {
  if (opened === port) opened = undefined;
  forget(port);
}

/** Change the target: the next request uses the session of the new one (and opens the port). A MIDI port that cannot be opened leaves the old target. */
export async function selectTarget(next: Target): Promise<void> {
  if (sameTarget(next, target())) return;
  const { targets } = transferEnv();
  if (next.kind === 'midi') await openTransport(next);
  else if (opened) await targets.close?.({ kind: 'midi', port: opened });
  cancelPending();
  if (opened) forget(opened);
  opened = next.kind === 'midi' ? next.port : undefined;
  if (next.kind === 'midi') {
    setLastMidiPort(next.port);
    try {
      localStorage.setItem(MIDI_KEY, next.port);
    } catch {
      // the choice is not kept
    }
  }
  setTargetSignal(next);
  noteConnection({ type: 'target' });
  try {
    localStorage.setItem(KEY, JSON.stringify(next));
  } catch {
    // the choice is not kept
  }
}

async function sessionOf(t: Target): Promise<DkSession> {
  const { targets } = transferEnv();
  if (t.kind === 'none')
    throw new Error(
      (await targets.whyNone?.()) ??
        'No MIDI port. Connect the device by USB, set its USB mode to MIDI, then pick it under Target.',
    );
  await targets.preflight?.(t);
  if (t.kind === 'midi') {
    if (opened !== t.port) {
      await openTransport(t);
      if (opened) forget(opened);
      opened = t.port;
    }
  } else if (!transports.has(targetKey(t))) {
    await openTransport(t);
  }
  return sessions.get(targetKey(t));
}

/** The one session of the selected target: its requests take turns on the one reply stream. A saved MIDI target is opened at its first use. */
export const session = (): Promise<DkSession> => sessionOf(target());

/**
 * The emulator's session whatever the selected target is (the Emulator page's Memory panel is about the emulator). It is THE emulator session,
 * the one `session()` returns while the emulator is the target, so every reader of the emulator's USB takes its turn in one queue.
 */
export const emulatorSession = (): Promise<DkSession> => sessionOf({ kind: 'emulator' });

/**
 * The session of the MIDI port chosen last, whatever the selected target is (the Log follows the device on it). The port is opened if it is
 * not; its requests take their turn with the Transfer pane's when the port is also the target.
 */
export function lastMidiSession(): Promise<DkSession> {
  const port = lastMidiPort();
  if (!port) return Promise.reject(new Error(NO_PORT));
  return sessionOf({ kind: 'midi', port });
}

/** Drop the session of a failed MIDI port (the next request opens it again). The emulator's session stays: it has no port to reopen. */
export function resetSession(): void {
  const t = target();
  if (t.kind === 'midi') sessions.drop(targetKey(t));
}

/** Give up on every request of the selected target's session that waits for the device (the Cancel button); each fails with the `cancelled` error. */
export function cancelPending(): void {
  sessions.cancel(targetKey(target()));
}

/**
 * The transport of the selected MIDI port, opened if it is not (the Firmware page sends its messages through it, one by one, without the
 * session). The port is the target's: a request of the session in the meantime would mix its reply with the page's.
 */
export async function portTransport(): Promise<DkTransport> {
  const t = target();
  if (t.kind !== 'midi') throw new Error('No MIDI port is selected.');
  await sessionOf(t);
  return transports.get(targetKey(t)) as DkTransport;
}

/** Let go of the selected MIDI port (the device restarts): the next request opens it again, and fails while it is not there. */
export async function releasePort(): Promise<void> {
  const t = target();
  if (t.kind !== 'midi') return;
  await transferEnv().targets.close?.(t);
  portLost(t.port);
}
