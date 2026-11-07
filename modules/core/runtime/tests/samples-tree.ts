import type { Prepared } from '@digi/dk-session/dksample';
import type { SamplesBackend } from '@digi/dk-session/samples';
import { liveCheck as check } from '@digi/regression/runtime/artifacts.ts';
import {
  applyUpload,
  uploadItems,
  type UploadEvent,
  type Uploads,
} from '../../../../apps/desktop/ui/src/samples-core.ts';

/** The files under `folder`, with their paths from the top, folders walked. */
async function files(dev: SamplesBackend, folder: string): Promise<string[]> {
  const out: string[] = [];
  for (const e of await dev.list(folder)) {
    const path = `${folder}/${e.name}`;
    if (e.kind === 'folder') out.push(...(await files(dev, path)));
    else out.push(path);
  }
  return out;
}

/**
 * `removeTree` on a real backend, in a scratch folder: `Foo` (nested) and `Foobar` (the same prefix) next to it; a stop after one file leaves
 * the rest listed, the second call finishes, `Foobar` stays intact until its own delete, and the free space follows.
 * `make(name)` is a tiny prepared sample.
 */
export async function expectRemoveTree(dev: SamplesBackend, make: (name: string) => Prepared): Promise<void> {
  const top = 'scratch-rt';
  const space0 = await dev.space();
  for (const [folder, name] of [
    ['Foo', 'one'],
    ['Foo/sub', 'two'],
    ['Foo/sub/deep', 'three'],
    ['Foobar', 'keep'],
  ] as const)
    await dev.put(`${top}/${folder}`, make(name));
  const space1 = await dev.space();
  check('the scratch files took space', space1.free < space0.free, `${space0.free} -> ${space1.free}`);

  let done = 0;
  const part = await dev.removeTree(`${top}/Foo`, { onProgress: (d) => (done = d), shouldStop: () => done >= 1 });
  check('a stop after one file: stopped, one of three removed', part.stopped && part.removed === 1 && part.total === 3, JSON.stringify(part));
  const rest = await files(dev, `${top}/Foo`);
  check('the other two files are still listed', rest.length === 2, rest.join(' '));

  const end = await dev.removeTree(`${top}/Foo`);
  check('the second call removes the rest', !end.stopped && end.removed === 2 && end.total === 2, JSON.stringify(end));
  check('Foo and its children are gone', !(await dev.list(top)).some((e) => e.name === 'Foo'));
  check('Foobar is still there with its file', (await files(dev, `${top}/Foobar`)).join() === `${top}/Foobar/keep`);
  check('Foobar verifies', (await dev.verify(`${top}/Foobar`, make('keep'))).ok);
  const space2 = await dev.space();
  check('the free space grew by what Foo held', space2.free > space1.free && space2.free < space0.free, `${space1.free} -> ${space2.free}`);

  await dev.removeTree(top);
  check('the scratch folder is gone', !(await dev.list('')).some((e) => e.name === top));
  const space3 = await dev.space();
  check('the free space is back within 64 KiB of the start', Math.abs(space3.free - space0.free) <= 64 * 1024, `${space0.free} -> ${space3.free}`);
}

/**
 * Resuming an upload on a real backend: the upload run of the Samples page (`uploadItems`) sends three files, sends nothing the second time (no
 * `put`, no VERIFY, one listing per folder), and after one file changed sends that file only. This is where a host hash that differs from the device's
 * would show: every file would go again. `wav` is a WAV; `make(name, wav)` is `prepareUpload` of it under `name`.
 */
export async function expectSkipOnUpload(
  dev: SamplesBackend,
  wav: Uint8Array,
  make: (name: string, wav: Uint8Array) => Prepared,
): Promise<void> {
  const folder = 'scratch-skip';
  const changed = Uint8Array.from(wav);
  changed[wav.length - 200] = changed[wav.length - 200]! ^ 0x40; // one byte of the audio
  const names = ['one', 'two', 'three'];
  const base = names.map((n) => make(n, wav));
  check(
    'the changed WAV converts to a file of the same size with other bytes',
    make('two', changed).stored.length === base[1]!.stored.length &&
      Buffer.compare(Buffer.from(make('two', changed).stored), Buffer.from(base[1]!.stored)) !== 0,
  );

  const calls: string[] = [];
  const counted = {
    put: (f: string, p: Prepared, o?: (sent: number, total: number) => void) => (calls.push(`put ${f}/${p.name}`), dev.put(f, p, o)),
    verify: (f: string, p: Prepared) => (calls.push(`verify ${f}/${p.name}`), dev.verify(f, p)),
    list: (f?: string) => (calls.push(`list ${f ?? ''}`), dev.list(f)),
    mkdir: (f: string) => (calls.push(`mkdir ${f}`), dev.mkdir(f)),
    sameOnDevice: dev.sameOnDevice.bind(dev),
  };
  const run = async (wavOf: (name: string) => Uint8Array): Promise<Uploads> => {
    calls.length = 0;
    let u: Uploads = {};
    const items = names.map((name) => ({ folder, name, size: 1, read: async () => wavOf(name) }));
    await uploadItems(
      counted,
      (name, bytes) => make(name, bytes),
      [folder],
      items,
      (e: UploadEvent) => (u = applyUpload(u, e)),
    );
    return u;
  };
  const states = (u: Uploads) => names.map((n) => u[`${folder}/${n}`]?.state).join(' ');
  const count = (kind: string) => calls.filter((c) => c.startsWith(`${kind} `)).length;

  const first = await run(() => wav);
  check('the first run sends and verifies all three', states(first) === 'ok ok ok' && count('put') === 3 && count('verify') === 3, `${states(first)} ${calls.join(', ')}`);

  const again = await run(() => wav);
  check('the second run sends nothing: all three are on the device', states(again) === 'same same same', states(again));
  check('no put and no VERIFY, and each folder is listed once', count('put') === 0 && count('verify') === 0 && count('list') <= 2 && count('mkdir') === 0, calls.join(', '));

  const one = await run((name) => (name === 'two' ? changed : wav));
  check('after one file changed only that file goes', states(one) === 'same ok same' && calls.filter((c) => c.startsWith('put')).join() === `put ${folder}/two`, `${states(one)} ${calls.join(', ')}`);
  check('the changed file is on the device now', (await dev.verify(folder, make('two', changed))).ok);
  check('and the unchanged one still is', (await dev.verify(folder, make('one', wav))).ok);

  await dev.removeTree(folder);
}
