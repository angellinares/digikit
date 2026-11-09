// A flash from the first message to the verdict, over operations the app gives (so the same run is tested with a fake device, run in the browser
// over Web MIDI, in the desktop app over midir, and run headless against the emulator). Rust paces it: `flash::Sender` (through patchkit.wasm)
// says when the next message goes out. The device sends no reply while it receives (finding 75), so the verdict comes from the restart:
// the port goes away and comes back, then INFO (runtime and image id) or, for a restore, SoftwareVersion.
import type { FlashSender, PreparedFlash, RpcMachine } from '../patch/patchkit-core';
import {
  CANCEL_TEXT,
  NO_ANSWER_TEXT,
  STOPPED_WAITING_TEXT,
  judge,
  noReturnText,
  samePortName,
  sendFailedText,
  type Answer,
  type FlashState,
} from './firmware-core';

export interface FlowOps {
  /** The Rust sender for this many messages. */
  sender(total: number): Promise<FlashSender>;
  /** One message out; throws when the port refuses it. */
  send(message: Uint8Array): Promise<void> | void;
  /** A millisecond clock that does not go back. */
  now(): number;
  sleep(ms: number): Promise<void>;
  /** Let go of the port: the device is about to restart. */
  release(): Promise<void>;
  /** The name of the device's port when the run started. */
  port: string;
  /** The names of the MIDI ports now. */
  ports(): Promise<string[]>;
  /** The device came back on a port with another name (CoreMIDI numbers a second port of one name): use this port from now on. */
  moved?(name: string): Promise<void> | void;
  /** One line for the app's log. */
  log?(text: string): void;
  /** Ask the device after the restart; throws while it does not answer. INFO that does not answer is `runtime: undefined`. */
  ask(): Promise<Answer>;
}

export interface FlowLimits {
  /** How long to wait for the port to go away after the last message; then the device is asked anyway (the restart may have been faster than a poll). */
  leaveMs: number;
  /** How long it may take to come back. */
  returnMs: number;
  /** How long the device may take to answer after it is back. */
  askMs: number;
  pollMs: number;
  askEveryMs: number;
  /** Real-time limits of one `release`, one `ports` and one `ask` (a call that hangs must not stop the run). */
  releaseMs: number;
  portsMs: number;
  askTryMs: number;
}

export const LIMITS: FlowLimits = {
  leaveMs: 10_000,
  returnMs: 240_000,
  askMs: 90_000,
  pollMs: 400,
  askEveryMs: 2000,
  releaseMs: 5000,
  portsMs: 5000,
  askTryMs: 10_000,
};

const messageOf = (p: PreparedFlash, i: number): Uint8Array => p.image.subarray(i === 0 ? 0 : p.ends[i - 1], p.ends[i]);

const text = (e: unknown): string => (e instanceof Error ? e.message : String(e));

/** The UI is told at most this often while sending. */
const UI_MS = 100;

/**
 * Send `p`, wait for the restart, check the device. `set` receives every state; the last one is also returned. `signal` stops the run: while
 * sending, the sender is told to stop (the device keeps its old image, finding 75); while waiting, the wait ends.
 */
export async function runFlash(
  ops: FlowOps,
  p: PreparedFlash,
  signal: AbortSignal,
  set: (s: FlashState) => void,
  limits: FlowLimits = LIMITS,
): Promise<FlashState> {
  const done = (s: FlashState): FlashState => {
    set(s);
    return s;
  };
  const total = p.ends.length;
  const sender = await ops.sender(total);
  const startedMs = ops.now();
  try {
    let shown = Number.NEGATIVE_INFINITY;
    set({ phase: 'sending', sent: 0, total, startedMs, nowMs: startedMs });
    for (;;) {
      if (signal.aborted) {
        sender.fail();
        return done({ phase: 'stopped', reason: 'cancelled', sent: sender.sent(), total, message: CANCEL_TEXT });
      }
      const now = ops.now();
      const step = sender.next(now);
      if (step.kind === 'send') {
        try {
          await ops.send(messageOf(p, step.index));
        } catch (e) {
          sender.fail();
          return done({
            phase: 'stopped',
            reason: 'error',
            sent: step.index,
            total,
            message: sendFailedText(step.index, total, text(e)),
          });
        }
        if (now - shown >= UI_MS || step.index + 1 === total) {
          shown = now;
          set({ phase: 'sending', sent: step.index + 1, total, startedMs, nowMs: now });
        }
      } else if (step.kind === 'wait') {
        await ops.sleep(step.ms);
      } else if (step.kind === 'done') {
        break;
      } else {
        return done({ phase: 'stopped', reason: 'error', sent: sender.sent(), total, message: 'The sender stopped.' });
      }
    }
  } finally {
    sender.free();
  }
  return afterSend(ops, p, signal, set, limits, false);
}

/** Reject-free timeout: the promise's value, or `undefined` when `ms` real milliseconds pass or the run is aborted. */
async function within<T>(p: Promise<T>, ms: number, signal: AbortSignal): Promise<{ value: T } | undefined> {
  let off = (): void => {};
  const out = new Promise<undefined>((resolve) => {
    const timer = setTimeout(() => resolve(undefined), ms);
    const abort = () => resolve(undefined);
    signal.addEventListener('abort', abort, { once: true });
    off = () => {
      clearTimeout(timer);
      signal.removeEventListener('abort', abort);
    };
  });
  try {
    return await Promise.race([p.then((value) => ({ value })), out]);
  } finally {
    off();
  }
}

/**
 * The device checks and writes by itself; its port goes away and comes back, then the device is asked. Shared by both routes (`rpc`: the
 * device accepted End with commit 1; menu: the last message was sent). No step can hang the run: `release`, `ports` and `ask` have a time
 * limit each, and the restart need not be seen (a restart faster than one poll is only asked for).
 */
async function afterSend(
  ops: Pick<FlowOps, 'now' | 'sleep' | 'release' | 'ports' | 'ask' | 'port' | 'moved' | 'log'>,
  p: PreparedFlash,
  signal: AbortSignal,
  set: (s: FlashState) => void,
  limits: FlowLimits,
  rpc: boolean,
): Promise<FlashState> {
  const done = (s: FlashState): FlashState => {
    set(s);
    return s;
  };
  const since = ops.now();
  let shown: string | undefined;
  let names: string[] | undefined;
  const stage = (s: 'leaving' | 'returning' | 'asking') =>
    set({ phase: 'rebooting', stage: s, sinceMs: since, nowMs: ops.now(), ...(rpc ? { rpc: true as const } : {}) });
  /** The ports now, or undefined when the list did not come in time ("unknown"). */
  const listPorts = async (): Promise<string[] | undefined> => {
    const r = await within(
      ops.ports().catch(() => undefined),
      limits.portsMs,
      signal,
    );
    if (r?.value) names = r.value;
    return r?.value;
  };
  const enter = async (s: 'leaving' | 'returning' | 'asking') => {
    stage(s);
    if (shown === s) return;
    shown = s;
    await listPorts();
    ops.log?.(`rebooting: ${s} · ports: ${names ? names.join(', ') || 'none' : 'unknown'}`);
  };
  stage('leaving');
  await within(
    ops.release().catch(() => undefined),
    limits.releaseMs,
    signal,
  );
  await enter('leaving');
  const mine = (list: string[]) => list.filter((n) => samePortName(ops.port, n));
  // wait for the port to go; a restart faster than a poll is not seen, so after `leaveMs` the device is asked anyway
  let left = false;
  const leaveEnd = ops.now() + limits.leaveMs;
  while (ops.now() < leaveEnd && !signal.aborted) {
    const list = await listPorts();
    if (list && mine(list).length === 0) {
      left = true;
      break;
    }
    await ops.sleep(limits.pollMs);
    stage('leaving');
  }
  if (signal.aborted) return done({ phase: 'failed', code: 'no-restart', message: STOPPED_WAITING_TEXT });
  if (left) {
    await enter('returning');
    const returnEnd = ops.now() + limits.returnMs;
    let back: string | undefined;
    while (!back && ops.now() < returnEnd && !signal.aborted) {
      const list = await listPorts();
      const found = list ? mine(list) : [];
      if (found.length) back = found.includes(ops.port) ? ops.port : found[0];
      else {
        await ops.sleep(limits.pollMs);
        stage('returning');
      }
    }
    if (!back)
      return done(
        signal.aborted
          ? { phase: 'failed', code: 'no-return', message: STOPPED_WAITING_TEXT }
          : { phase: 'failed', code: 'no-return', message: noReturnText(ops.port) },
      );
    if (back !== ops.port) {
      ops.log?.(`the device came back as ${back} (it was ${ops.port})`);
      try {
        await ops.moved?.(back);
      } catch (e) {
        ops.log?.(`could not switch to ${back}: ${text(e)}`);
      }
    }
  }
  await enter('asking');
  const end = ops.now() + limits.askMs;
  let stockSeen = 0;
  let last: Answer | undefined;
  const verdict = (a: Answer): FlashState => {
    const v = judge(p, a);
    return v.ok
      ? { phase: 'done', message: v.message, lines: v.lines }
      : { phase: 'failed', code: v.code, message: v.message };
  };
  while (ops.now() < end && !signal.aborted) {
    await ops.sleep(limits.askEveryMs);
    const r = await within(
      ops.ask().catch(() => undefined),
      limits.askTryMs,
      signal,
    );
    const a = r?.value;
    if (!a) {
      stage('asking');
      continue;
    }
    // A restore ends when the stock firmware answers and no runtime does, twice in a row: a patched image answers SoftwareVersion
    // a little before its runtime answers INFO.
    last = a;
    if (p.restore && !a.runtime && a.os === p.os && ++stockSeen < 2) continue;
    // a patched image answers SoftwareVersion before its runtime answers INFO: wait for the runtime
    if (!p.restore && !a.runtime) continue;
    // The old image can still answer for a moment (also when the port never seemed to leave): keep asking while the answer is not
    // the patch's, and judge the last answer when the time is up.
    if (!p.restore && (a.runtime !== p.runtime || (a.imageId ?? null) !== p.imageId)) continue;
    return done(verdict(a));
  }
  if (last && !signal.aborted) return done(verdict(last));
  return done({
    phase: 'failed',
    code: 'no-answer',
    message: signal.aborted ? 'You stopped waiting.' : NO_ANSWER_TEXT,
  });
}

// ---- the RPC route ---------------------------------------------------------------------------------------------------------------

/** The operations of the RPC route: the state machine (Rust, `flash_rpc::RpcFlash`) and a port that sends one message and reads the next. */
export interface RpcOps extends Pick<
  FlowOps,
  'send' | 'now' | 'sleep' | 'release' | 'ports' | 'ask' | 'port' | 'moved' | 'log'
> {
  machine(image: Uint8Array, chunk: number): Promise<RpcMachine>;
  /** The next message the device sent within `timeoutMs`, or null; ends early (null) when `signal` aborts. */
  recv(timeoutMs: number, signal?: AbortSignal): Promise<Uint8Array | null>;
}

/**
 * Send the image through the RPC route, commit when the device has checked it, and wait for the restart. All decisions (what to send, how often to repeat a chunk, when a
 * run has failed) are the Rust machine's; this loop moves bytes and the clock. `signal` aborts: before End with commit 1 the device gets
 * End with commit 0 and keeps its flash. `chunk` 0 is the default size.
 */
export async function runRpcFlash(
  ops: RpcOps,
  p: PreparedFlash,
  signal: AbortSignal,
  set: (s: FlashState) => void,
  limits: FlowLimits = LIMITS,
  chunk = 0,
): Promise<FlashState> {
  const done = (s: FlashState): FlashState => {
    set(s);
    return s;
  };
  const total = p.image.length;
  const m = await ops.machine(p.image, chunk);
  const startedMs = ops.now();
  let rebooting = false;
  try {
    let shown = Number.NEGATIVE_INFINITY;
    let cancelled = false;
    let portError = '';
    const progress = (checking: boolean) => {
      const now = ops.now();
      if (checking || now - shown >= UI_MS || m.taken() === total) {
        shown = now;
        set({
          phase: 'sending',
          sent: m.taken(),
          total,
          startedMs,
          nowMs: now,
          unit: 'bytes',
          ...(checking ? { checking: true } : {}),
        });
      }
    };
    set({ phase: 'sending', sent: 0, total, startedMs, nowMs: startedMs, unit: 'bytes' });
    for (;;) {
      if (signal.aborted && !cancelled) {
        cancelled = true;
        m.cancel();
      }
      const a = m.next();
      if (a.kind === 'send') {
        // the long timeout is the last Write: the device checks the whole image before it answers
        if (a.timeoutMs >= 10_000 && !cancelled) progress(true);
        try {
          await ops.send(a.bytes);
        } catch (e) {
          // a port that refuses the message cannot be asked for anything else: end the exchange as a timeout
          portError = text(e);
          m.timeout();
          continue;
        }
        const end = ops.now() + a.timeoutMs;
        // wait for the reply to this request: the message is ours (the machine says so), or the time is up, or the user aborts
        const wait = async (sig: AbortSignal | undefined): Promise<'got' | 'timeout' | 'aborted'> => {
          for (let left = end - ops.now(); left > 0; left = end - ops.now()) {
            const r = await ops.recv(left, sig);
            if (r && m.reply(r)) return 'got';
            if (!r && sig?.aborted) return 'aborted';
          }
          return 'timeout';
        };
        let r = await wait(cancelled ? undefined : signal);
        if (r === 'aborted') {
          cancelled = true;
          // End with commit 0 follows. When it is too late (End with commit 1 is in flight) the reply is still awaited.
          if (m.cancel()) continue;
          r = await wait(undefined);
        }
        if (r === 'timeout') m.timeout();
        if (r === 'got') progress(false);
      } else if (a.kind === 'ask') {
        // the Flash click was the yes: the device validated the image, so the commit follows at once (a cancel that arrived until now still wins)
        set({ phase: 'sending', sent: m.taken(), total, startedMs, nowMs: ops.now(), unit: 'bytes', flashing: true });
        m.confirm(!signal.aborted);
      } else if (a.kind === 'reboot') {
        rebooting = true;
        break;
      } else if (a.kind === 'done') {
        break;
      } else {
        const reason = a.code === 'cancelled' ? 'cancelled' : 'error';
        return done({
          phase: 'stopped',
          reason,
          sent: a.taken,
          total,
          unit: 'bytes',
          message: portError && reason === 'error' ? `${a.message} (${portError})` : a.message,
        });
      }
    }
  } finally {
    m.free();
  }
  return rebooting
    ? afterSend(ops, p, signal, set, limits, true)
    : done({ phase: 'failed', code: 'send', message: 'The device did not start the flash.' });
}
