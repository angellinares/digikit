import type { CallResult, MaskedPoke, Op, Poke, RigPort, RigSetup } from './rig.ts';
import { openRig } from './rig.ts';

/** Opens the rig for a setup: the real process by default; a host test passes a fake. */
export type RigOpener = (setup: RigSetup) => Promise<{ port: RigPort; first: CallResult[] }>;
export const realRig: RigOpener = openRig;

const u32 = (n: number): number => n >>> 0;

/** The ops a rig has collected since its last call. Neighbouring pokes of one kind share an op, so the order of the ops is the order of the calls to this class. */
export class OpQueue {
  private ops: Op[] = [];

  poke(address: number, word: number): void {
    const last = this.ops.at(-1);
    const pair: Poke = [address, u32(word)];
    if (last && 'poke' in last) last.poke.push(pair);
    else this.ops.push({ poke: [pair] });
  }

  /** Change the bits of MASK in the word at ADDRESS to those of VALUE. */
  pokeMasked(address: number, mask: number, value: number): void {
    const last = this.ops.at(-1);
    const triple: MaskedPoke = [address, u32(mask), u32(value)];
    if (last && 'pokem' in last) last.pokem.push(triple);
    else this.ops.push({ pokem: [triple] });
  }

  push(op: Op): void {
    this.ops.push(op);
  }

  /** The ops so far; the queue is empty afterwards. */
  take(): Op[] {
    const ops = this.ops;
    this.ops = [];
    return ops;
  }
}

/** 16 bits of a DM word: halfword K of an array of halfwords that starts at word address BASE (the frame copy). */
export function pokeHalf(q: OpQueue, base: number, k: number, value: number): void {
  const shift = 16 * (k % 2);
  q.pokeMasked(base + Math.floor(k / 2), 0xffff << shift, (value & 0xffff) << shift);
}

/** The text `0x1f` of a number (Python's `{n:#x}`: a negative number is `-0x1`). */
export const hex = (n: number): string => (n < 0 ? `-0x${(-n).toString(16)}` : `0x${n.toString(16)}`);
