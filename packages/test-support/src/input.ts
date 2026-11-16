import { waitClock, type Clock } from './emulated-clock.ts';
import { button, encoder } from './keys.ts';
import { MS, type Session } from './session.ts';

/** A panel key or encoder: its name or its code. */
export type Key = string | number;

/** One input path. `tap` returns after the release has happened in emulated time. */
export interface Input {
  readonly name: 'panel' | 'probe' | 'remote';
  button(key: Key, down: boolean): Promise<void>;
  tap(key: Key, holdMs: number): Promise<void>;
  turn(encoder: Key, detents: number): Promise<void>;
}

/** Press, hold, release (also when the press or the hold fails). */
async function holdTap(send: (down: boolean) => Promise<unknown>, clock: Clock, holdMs: number): Promise<void> {
  try {
    await send(true);
    await waitClock(clock, holdMs);
  } finally {
    await send(false);
  }
}

/** The daemon's virtual panel: `tap` schedules the release in the daemon and waits until emulated time passed it. */
export function panelInput(session: Pick<Session, 'status' | 'client'>, clock: Clock): Input {
  const { client } = session;
  return {
    name: 'panel',
    async button(key, down) {
      await client.inputButton(button(key), down);
    },
    async tap(key, holdMs) {
      const code = button(key);
      try {
        const start = await clock.now();
        const reply = (await client.inputTap(code, Math.floor(holdMs * MS))) as
          { release_at?: number } | null | undefined;
        const target = typeof reply?.release_at === 'number' ? reply.release_at / MS + 5 : start + holdMs + 5;
        while ((await clock.now()) < target) await clock.pause(20);
      } catch (error) {
        await client.inputButton(code, false).catch(() => {});
        throw error;
      }
    },
    async turn(enc, detents) {
      await client.inputTurn(encoder(enc), detents);
    },
  };
}

/** The runtime's input probe (request 0x38 [code, down]); the reply status must be 0. */
export function probeInput(
  dk: { request(cmd: number, body: Uint8Array | Buffer): Promise<{ status: number }> },
  clock: Clock,
): Input {
  const send = async (key: Key, down: boolean) => {
    const { status } = await dk.request(0x38, Uint8Array.of(button(key), down ? 1 : 0));
    if (status !== 0) throw new Error(`input probe ${key} ${down ? 'down' : 'up'}: status ${status}`);
  };
  return {
    name: 'probe',
    button: send,
    tap: (key, holdMs) => holdTap((down) => send(key, down), clock, holdMs),
    async turn() {
      throw new Error('the input probe does not support turn: not supported');
    },
  };
}

/** digikit/remote (also works on hardware). */
export function remoteInput(
  dk: {
    remoteKey(code: number, down: boolean): Promise<unknown>;
    remoteTurnOnce(encoder: number, delta: number): Promise<number>;
  },
  clock: Clock,
): Input {
  const send = async (key: Key, down: boolean) => {
    await dk.remoteKey(button(key), down);
  };
  return {
    name: 'remote',
    button: send,
    tap: (key, holdMs) => holdTap((down) => send(key, down), clock, holdMs),
    async turn(enc, detents) {
      await dk.remoteTurnOnce(encoder(enc), detents);
    },
  };
}

/**
 * Scheduled encoder turns on the daemon's input clock. The stock encoder accelerates by event rate, so a fast burst moves a value much
 * further than the same turns with a wait between them. Both calls wait in emulated time on `clock`.
 */
export function scheduledTurns(session: Pick<Session, 'client'>, clock: Clock) {
  const { client } = session;
  async function untilMs(target: number): Promise<void> {
    while ((await clock.now()) < target) await clock.pause(20);
  }
  /** `count` turns of `detents` each, `gapMs` apart. Returns after the last event plus 30 ms. */
  async function burst(name: string, count: number, detents = 4, gapMs = 7.5): Promise<void> {
    const start = (await clock.now()) + 5;
    const code = encoder(name);
    const events = Array.from({ length: count }, (_, i) => ({
      at_instr: Math.round((start + i * gapMs) * MS),
      turn: code,
      detents,
    }));
    await client.inputSchedule(events);
    await untilMs(events.at(-1)!.at_instr / MS + 30);
  }
  /** Burst until `read()` is within `tolerance` of `target`, turning back when it overshoots. Returns the value reached and the bursts used. */
  async function turnTo(
    name: string,
    read: () => Promise<number>,
    target: number,
    { tolerance = 0x40, perBurst = 10, maxBursts = 12 } = {},
  ): Promise<{ value: number; bursts: number; trace: number[] }> {
    const trace = [await read()];
    for (let bursts = 0; bursts < maxBursts; bursts++) {
      const value = trace.at(-1)!;
      if (Math.abs(value - target) <= tolerance) return { value, bursts, trace };
      // Fewer events near the target: about 0x100 per event at this rate.
      const events = Math.max(1, Math.min(perBurst, Math.floor(Math.abs(target - value) / 0x180)));
      await burst(name, events, value < target ? 4 : -4);
      await waitClock(clock, 100); // the stock value settles after the last event (nothing observable)
      trace.push(await read());
    }
    const value = trace.at(-1)!;
    if (Math.abs(value - target) > tolerance)
      throw new Error(`turnTo ${name}: ${target.toString(16)} not reached: ${trace.map((v) => v.toString(16))}`);
    return { value, bursts: maxBursts, trace };
  }
  return { burst, turnTo };
}
