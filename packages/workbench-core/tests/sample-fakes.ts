// The fake samples backends and prepared files shared by the sample upload and Samples page tests.
import type { Prepared } from '@digi/dk-session/dksample';
import type { SampleEntry } from '@digi/dk-session/samples';
import {
  applyUpload,
  uploadItems,
  type UploadEvent,
  type UploadItem,
  type Uploads,
} from '../src/transfer/sample-upload.ts';

/** A backend that keeps what it is given; `failPut` names a file whose put throws, `failVerify` one whose verify says no. */
export type Backend = Parameters<typeof uploadItems>[0];
type Verify = Backend['verify'];
export const fakeBackend = ({ failPut = '', failVerify = '' } = {}) => {
  const calls: string[] = [];
  return {
    calls,
    async put(folderName: string, p: Prepared, onProgress: (sent: number, total: number) => void) {
      calls.push(`put ${folderName}/${p.name}`);
      if (p.name === failPut) throw new Error('{"error":"E_FULL: no space","kind":"device"}');
      onProgress(p.stored.length / 2, p.stored.length);
      onProgress(p.stored.length, p.stored.length);
    },
    async verify(folderName: string, p: Prepared) {
      calls.push(`verify ${folderName}/${p.name}`);
      return {
        ok: p.name !== failVerify,
        message: p.name === failVerify ? 'device hash differs' : 'device hash matches',
        compared: 'hash of the stored file',
      } as Awaited<ReturnType<Verify>>;
    },
  };
};
export const prepare = (name: string, data: Uint8Array): Prepared => {
  if (name.endsWith('.bad.wav')) throw new Error('not a usable WAV: no fmt chunk');
  const wav = name.endsWith('.wav');
  return {
    name: wav ? name.replace(/\.wav$/, '') : name,
    bytes: data,
    stored: data,
    kind: wav ? 'converted' : 'unchanged',
  };
};

export const cancelError = () => Object.assign(new Error('cancelled'), { kind: 'cancelled' });
export const names = (u: Uploads) => Object.values(u).map((x) => [x.name, x.state]);

/** A backend with a device: `have` is folder -> name -> bytes. `sameOnDevice` compares size and bytes as the real backends compare size and hash. */
export const deviceBackend = (
  have: Record<string, Record<string, Uint8Array>> = {},
  { failList = null, cancelList = null }: { failList?: string | null; cancelList?: string | null } = {},
) => {
  const calls: string[] = [];
  const base = fakeBackend();

  return {
    calls,
    put: async (f: string, p: Prepared, o: (sent: number, total: number) => void) => {
      calls.push(`put ${f}/${p.name}`);
      return base.put(f, p, o);
    },
    verify: async (f: string, p: Prepared) => {
      calls.push(`verify ${f}/${p.name}`);
      return base.verify(f, p);
    },
    async list(f: string): Promise<SampleEntry[]> {
      calls.push(`list ${f}`);
      if (f === failList) throw new Error('{"error":"E_BUSY: card busy","kind":"device"}');
      if (f === cancelList) throw cancelError();
      return Object.entries(have[f] ?? {}).map(([name, b]): SampleEntry => ({
        name,
        kind: 'file',
        size: b.length,
        hash: b.join(','),
      }));
    },
    sameOnDevice: (p: Prepared, listing: SampleEntry[]) =>
      listing.some((e) => e.name === p.name && e.size === p.stored.length && e.hash === p.stored.join(',')),
    async mkdir(f: string) {
      calls.push(`mkdir ${f}`);
    },
  };
};
export const bytesOf = (n: number, fill: number) => new Uint8Array(n).fill(fill);
export const filled = (folderPath: string, name: string, n: number, fill: number): UploadItem => ({
  folder: folderPath,
  name,
  size: n,
  read: async () => bytesOf(n, fill),
});
export const runAll = async (backend: Backend, mkdirs: string[], items: UploadItem[], stop?: () => boolean) => {
  let u: Uploads = {};
  const events: UploadEvent[] = [];
  await uploadItems(
    backend,
    prepare,
    mkdirs,
    items,
    (e) => {
      events.push(e);
      u = applyUpload(u, e);
    },
    stop,
    { settleMs: 0, commitCancelMs: 50 },
  );
  return { u, events };
};

/**
 * A backend with `commit` (the bulk backend of a device with the batch commit): `pending` says what each put reports (in order; the last repeats),
 * `commitFails` makes the commit throw, `fallbackAfter` flips `bulkState` after that many puts (the link was lost).
 */
export const batchBackend = ({
  pending = [1, 2, 3],
  commitFails = '',
  fallbackAfter = Infinity,
  commitLeft = [0],
  commitHangs = false,
  have = {} as Record<string, Record<string, Uint8Array>>,
}: {
  pending?: number[];
  commitFails?: string;
  fallbackAfter?: number;
  commitLeft?: number[];
  commitHangs?: boolean;
  have?: Record<string, Record<string, Uint8Array>>;
} = {}) => {
  const calls: string[] = [];
  const base = deviceBackend(have);
  let n = 0;
  let commits = 0;
  return {
    calls,
    list: base.list,
    sameOnDevice: base.sameOnDevice,
    bulkState: 'bulk' as 'bulk' | 'fallback',
    async put(f: string, p: Prepared, o: (sent: number, total: number) => void, opts?: { defer?: boolean }) {
      calls.push(`put ${f}/${p.name}${opts?.defer ? ' defer' : ''}`);
      await base.put(f, p, o);
      n++;
      if (n >= fallbackAfter) this.bulkState = 'fallback';
      return opts?.defer && this.bulkState === 'bulk'
        ? { pending: pending[Math.min(n, pending.length) - 1]! }
        : undefined;
    },
    verify: async (f: string, p: Prepared) => {
      calls.push(`verify ${f}/${p.name}`);
      return base.verify(f, p);
    },
    async commit(signal?: AbortSignal) {
      calls.push('commit');
      if (commitFails) throw new Error(commitFails);
      if (commitHangs)
        await new Promise<void>((_, reject) =>
          signal?.addEventListener('abort', () => reject(cancelError()), { once: true }),
        );
      return commitLeft[Math.min(commits++, commitLeft.length - 1)]!;
    },
  };
};
export const items3 = [filled('d', 'a', 10, 1), filled('d', 'b', 10, 2), filled('d', 'c', 10, 3)];
