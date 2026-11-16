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
