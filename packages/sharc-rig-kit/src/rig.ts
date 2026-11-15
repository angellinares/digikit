/**
 * The client of the SHARC-only rig (crate packages/sharc-rig): the request types and one `sharc-rig --serve` process per rig.
 * The rig knows nothing of machines or effects: it loads code, pokes DM words and runs calls on the generated core, and keeps
 * the core between the batches of ops (format: the module comment of packages/sharc-rig/src/lib.rs).
 */
import { spawn } from 'node:child_process';
import { existsSync } from 'node:fs';
import path from 'node:path';
import { createInterface } from 'node:readline';
import { fileURLToPath } from 'node:url';

/** The repository the kit reads sources, SDK headers and device files from: this one, or `DIGI_RIG_ROOT` (another checkout, read only: a profile of a branch that lives in a lane). */
export const ROOT = process.env.DIGI_RIG_ROOT
  ? path.resolve(process.env.DIGI_RIG_ROOT)
  : path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../..');

export type Poke = [address: number, word: number];
/** A read-modify-write of a DM word: `(old & ~mask) | (value & mask)`; a word that nothing wrote reads as 0. */
export type MaskedPoke = [address: number, mask: number, value: number];

export interface CallSpec {
  pc: number;
  regs: Record<string, number>;
  max_steps: number;
  reads: [address: number, count: number][];
  read_regs: string[];
  /** count the instructions executed at each pc (the result's `profile`) */
  profile?: boolean;
  /** `[loader byte address, count]` of the stock image as loaded (the result's `stock`); needs `RigSetup.image` */
  stock_reads?: [address: number, count: number][];
  /** pcs that end this call instead of the setup's `breaks` */
  breaks?: number[];
  /** continue from the pc the last call stopped at: no new frame, the stacks and registers stay (`regs` are still set) */
  resume?: boolean;
  /** 80-bit multiplier registers set before the call: `[mask, bits]` as hex strings (`MR`: bits known where the mask is 1) */
  specials?: Record<string, [mask: string, bits: string]>;
  /** multiplier registers (MRF, MRB, MSF, MSB) answered after the call (the result's `specials`) */
  read_specials?: string[];
}

export type Op = { poke: Poke[] } | { pokem: MaskedPoke[] } | { call: CallSpec };

export interface CallResult {
  instructions: number;
  /** `breakpoint`, `max-steps` or `trap: <reason>` */
  halt: string;
  final_pc: number;
  regs: Record<string, number | null>;
  /** one row per read of the call; `null` for a word that nothing wrote */
  reads: (number | null)[][];
  /** runs `[pc, address, width]` stored by instructions at `watch` pcs */
  stores: [pc: number, address: number, width: number][];
  /** `[pc, count]` per executed pc, in pc order; only for a call with `profile` */
  profile?: [pc: number, count: number][];
  /** one hex string per `stock_reads` entry (`null` when the image does not write every byte); only for a call with `stock_reads` */
  stock?: (string | null)[];
  /** `[mask, bits]` hex per `read_specials` name (`null` when the core does not hold the register); only for a call with `read_specials` */
  specials?: Record<string, [mask: string, bits: string] | null>;
}

export interface RigSetup {
  return_sw: number;
  breaks: number[];
  /** `[first pc, end pc]` ranges whose stores the calls report */
  watch?: [number, number][];
  /** the stock DSP loader stream (firmware section 7), hex: loaded before `code`, which goes over it */
  image?: string;
  /** loader byte address and boot-stream hex of each linked object */
  code: { address: number; hex: string }[];
  ops?: Op[];
}

/** What a rig needs from its transport: the real process, or a fake in a host test. */
export interface RigPort {
  run(ops: Op[]): Promise<CallResult[]>;
  close(): Promise<void>;
}

/** The rig binary: `DIGI_SHARC_RIG`, else dist/bin/sharc-rig (`nx run sharc-rig:build`). */
export function rigBinary(): string {
  const binary = process.env.DIGI_SHARC_RIG || path.join(ROOT, 'dist/bin/sharc-rig');
  if (!existsSync(binary)) throw new Error(`missing ${binary}: run nx run sharc-rig:build`);
  return binary;
}

interface Pending {
  resolve(line: string): void;
  reject(error: Error): void;
}

/** Start `sharc-rig --serve` and send the setup line; `setup.ops` run at once and their results come back from `run`'s first call. */
export async function openRig(setup: RigSetup): Promise<{ port: RigPort; first: CallResult[] }> {
  const child = spawn(rigBinary(), ['--serve'], { stdio: ['pipe', 'pipe', 'pipe'] });
  let stderr = '';
  child.stderr.on('data', (chunk: Buffer) => (stderr += chunk.toString()));
  const waiting: Pending[] = [];
  let ended: Error | null = null;
  const lines = createInterface({ input: child.stdout });
  lines.on('line', (line) => waiting.shift()?.resolve(line));
  const end = (error: Error) => {
    ended ??= error;
    for (const p of waiting.splice(0)) p.reject(error);
  };
  child.on('error', (error) => end(error));
  const exited = new Promise<void>((resolve) =>
    child.on('close', (code) => {
      end(new Error(`sharc-rig exited with ${code}: ${stderr.trim()}`));
      resolve();
    }),
  );
  const send = (value: unknown): Promise<CallResult[]> =>
    new Promise<string>((resolve, reject) => {
      if (ended) return reject(ended);
      waiting.push({ resolve, reject });
      child.stdin.write(`${JSON.stringify(value)}\n`);
    }).then((line) => {
      const answer = JSON.parse(line) as { error?: string; results?: CallResult[] };
      if (answer.error !== undefined) throw new Error(`sharc-rig: ${answer.error}`);
      return answer.results ?? [];
    });
  const first = await send(setup).catch(async (error: unknown) => {
    child.kill();
    await exited;
    throw error;
  });
  const port: RigPort = {
    run: (ops) => send({ ops }),
    async close() {
      child.stdin.end();
      await exited;
    },
  };
  return { port, first };
}
