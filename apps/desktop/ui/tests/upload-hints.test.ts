// uploadItems hands the backend what it already knows (the listing: does the file exist) and the user's stop as an AbortSignal, so a bulk upload replaces
// atomically without a second listing and a cancel reaches a send in flight.
import { expect, test } from 'vitest';
import type { Prepared } from '@digi/dk-session/dksample';
import type { SampleEntry, SamplesBackend } from '@digi/dk-session/samples';
import { uploadItems, type UploadEvent } from '../src/samples-core.ts';

const prep = (name: string): Prepared => ({
  name,
  bytes: new Uint8Array(4),
  stored: new Uint8Array(4),
  kind: 'unchanged',
});

function backend(
  listing: SampleEntry[],
  onPut: (name: string, opts?: { signal?: AbortSignal; exists?: boolean }) => Promise<void>,
) {
  let lists = 0;
  const b = {
    list: async () => (lists++, listing),
    sameOnDevice: () => false,
    put: async (_f: string, p: Prepared, _cb: unknown, opts?: { signal?: AbortSignal; exists?: boolean }) =>
      onPut(p.name, opts),
    verify: async () => ({ ok: true, message: 'ok', compared: '' }),
  };
  return { b: b as unknown as Pick<SamplesBackend, 'put' | 'verify' | 'list' | 'sameOnDevice'>, lists: () => lists };
}

test('the put gets the existence from the listing the run has, and the folder is listed once', async () => {
  const seen: [string, boolean | undefined][] = [];
  const { b, lists } = backend(
    [{ name: 'old', kind: 'file', size: 4, hash: 'x' }],
    async (n, o) => void seen.push([n, o?.exists]),
  );
  const events: UploadEvent[] = [];
  await uploadItems(
    b,
    (name) => prep(name),
    [],
    ['old', 'new'].map((name) => ({ folder: '', name, size: 4, read: async () => new Uint8Array(4) })),
    (e) => events.push(e),
  );
  expect(seen).toEqual([
    ['old', true],
    ['new', false],
  ]);
  expect(lists()).toBe(1);
});

test('the stop of the run aborts the signal of a put in flight', async () => {
  let stopped = false;
  const states: boolean[] = [];
  const { b } = backend([], async (_n, o) => {
    stopped = true;
    await new Promise<void>((r) => o!.signal!.addEventListener('abort', () => r(), { once: true }));
    states.push(o!.signal!.aborted);
    throw Object.assign(new Error('cancelled'), { kind: 'cancelled' });
  });
  const events: UploadEvent[] = [];
  await uploadItems(
    b,
    (name) => prep(name),
    [],
    [{ folder: '', name: 'a', size: 4, read: async () => new Uint8Array(4) }],
    (e) => events.push(e),
    () => stopped,
  );
  expect(states).toEqual([true]);
  expect(events.some((e) => e.event === 'cancelled')).toBe(true);
});
