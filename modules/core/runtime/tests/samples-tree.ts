import type { Prepared } from '@digi/dk-session/dksample';
import type { SamplesBackend } from '@digi/dk-session/samples';
import { liveCheck as check } from '@digi/regression/runtime/artifacts.ts';

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
