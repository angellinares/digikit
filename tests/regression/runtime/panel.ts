import { button, encoder, type Session } from '@digi/test-support';
import { expect } from 'vitest';

/** ColdFire instructions per emulated millisecond. */
const MS = 132_000;

/** Input scheduling uses elapsed, the daemon's own input clock, not wall time. */
export function panel(session: Session) {
  async function elapsed(): Promise<number> {
    const value = (await session.status()).elapsed;
    if (typeof value !== 'number' || !Number.isSafeInteger(value)) throw new Error('input requires status.elapsed');
    return value;
  }
  async function until(target: number): Promise<void> {
    await expect.poll(elapsed, { timeout: 20_000, interval: 50 }).toBeGreaterThanOrEqual(target);
  }
  async function settle(ms: number): Promise<void> {
    await until((await elapsed()) + ms * MS);
  }
  async function tap(name: string | number, settleMs = 80): Promise<void> {
    const result = (await session.client.inputTap(button(name), 20 * MS)) as { release_at?: number };
    if (!Number.isSafeInteger(result.release_at)) throw new Error('input.tap must report release_at');
    await until(result.release_at! + 5 * MS);
    await settle(settleMs);
  }
  async function chord(held: string, key: string | number): Promise<void> {
    await session.client.inputButton(button(held), true);
    try {
      await settle(5);
      await tap(key);
    } finally {
      await session.client.inputButton(button(held), false);
    }
    await settle(300);
  }
  async function turn(name: string, detents: number): Promise<void> {
    await session.client.inputTurn(encoder(name), detents);
    await settle(200);
  }
  /** `count` turns of `detents` each, `gapMs` apart, scheduled on the input clock. The stock encoder accelerates by event rate, so a
   * fast burst moves a value much further than the same turns with a settle between them. Returns after the last event plus 30 ms. */
  async function burst(name: string, count: number, detents = 4, gapMs = 7.5): Promise<void> {
    const start = (await elapsed()) + 5 * MS;
    const code = encoder(name);
    const events = Array.from({ length: count }, (_, i) => ({
      at_instr: Math.round(start + i * gapMs * MS),
      turn: code,
      detents,
    }));
    await session.client.inputSchedule(events);
    await until(events.at(-1)!.at_instr + 30 * MS);
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
      await settle(100);
      trace.push(await read());
    }
    const value = trace.at(-1)!;
    if (Math.abs(value - target) > tolerance)
      throw new Error(`turnTo ${name}: ${target.toString(16)} not reached: ${trace.map((v) => v.toString(16))}`);
    return { value, bursts: maxBursts, trace };
  }
  return { tap, chord, turn, settle, burst, turnTo };
}

/** Read-only ColdFire and DSP memory access over the control socket. */
export function memory(session: Session) {
  async function cf(addr: number): Promise<number> {
    const reply = (await session.client.call('mem.cf.peek', { addr })) as { value: number };
    return reply.value >>> 0;
  }
  async function words(addr: number, count: number): Promise<number[]> {
    const values = [];
    for (let i = 0; i < count; i++) values.push(await cf(addr + i * 4));
    return values;
  }
  async function dsp(addr: number, len: number) {
    return (await session.client.call('dsp.peek', { addr, len })) as {
      addr: number;
      len: number;
      hex: string;
      unknown: number;
    };
  }
  /** Halfword at a ColdFire address (big endian, read through the aligned word). */
  async function half(addr: number): Promise<number> {
    const word = await cf(addr & ~3);
    return (addr & 2 ? word : word >>> 16) & 0xffff;
  }
  /** Value of stock sound parameter `index` (the mirror index, 25 + knob slot for the machine knobs) of `track` on DT2: the 16 Sound objects lie
   * inline in the kit object, track t at `*0x80004704 + 0x34 + 0x450 t` (finding 64; the pointer table at 0x8000470c holds 0 for track 0), and the
   * parameter halfwords start 0x14 into the object (finding 02). The array holds zeros until the track's first trig. */
  async function soundParam(track: number, index: number): Promise<number> {
    return half((await cf(0x80004704)) + 0x34 + 0x450 * track + 0x14 + 2 * index);
  }
  return { cf, words, dsp, half, soundParam };
}
