import type { Session } from '@digi/test-support';

/** The word-aligned address at or below ADDRESS, unsigned: `& ~3` alone is a signed 32-bit result, negative from 0x80000000 up (the DN2 voice words). */
export const wordBase = (address: number): number => (address & ~3) >>> 0;

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
    const word = await cf(wordBase(addr));
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
