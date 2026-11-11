import { expect } from 'vitest';
import { fileHashHex } from '@digi/dk-session';
import { LAYOUTS, PAGE, dn2Tests, physOf, plane, planePages, FOLDER, type Dn2 } from './shot-dn2-support.ts';

// DN2 OS 1.12, store read leases (docs/plans/usb-fast.md step 1b): while the sample pool loads a store file, the runtime refuses to replace it, delete it or
// clear the store (E_FILE_IN_USE on SysEx), a new file can still be stored, and the pool's copy in DDR is the file as it was. When the load is done the
// lease is gone: replace and delete work.

dn2Tests('DN2 store lease: a file that the pool loads cannot be replaced or deleted', 'shot-dn2-lease', ['q', 't'], async (c: Dn2) => {
  const { dk, api, settle, log, phase, pool, rec, ddr, fixtures, file } = c;
  const q = fixtures.q!;
  const t = fixtures.t!;
  const path = `samples/${FOLDER}/q`;
  const other = `samples/${FOLDER}/n`;
  const hashOf = async (p: string) => (await dk.list()).find((e) => e.path === p)?.hash;
  const qHash = fileHashHex(q.prep.stored);

  await phase('lease', async () => {
    expect(await hashOf(path), 'the fixture is stored where the test expects it').toBe(qHash);
    const p0 = await pool();
    expect(await api.select(file('q'), 0), 'select queues the load').toBe(0);
    // wait until pages flow: the load is running and holds its lease
    let p = p0;
    const t0 = Date.now();
    while (p.pages - p0.pages < 8) {
      if (Date.now() - t0 > 60_000) throw new Error('the load did not start');
      p = await pool();
      await new Promise((r) => setTimeout(r, 30));
    }
    const refusals: Record<string, string | undefined> = {};
    const refused = async (name: string, op: () => Promise<unknown>) => {
      refusals[name] = await op().then(
        () => 'accepted',
        (e: unknown) => (e as { status?: string }).status ?? String(e),
      );
    };
    await refused('replace', () => dk.put(path, t.prep.stored));
    await refused('delete', () => dk.delete(path));
    await refused('clear', () => dk.clear());
    const mid = await api.status(0);
    const pMid = await pool();
    log.lease = { refusals, stateAfterRefusals: mid.state, pagesAfterRefusals: pMid.pages - p0.pages, expectedPages: 2 * planePages(q.frames) };
    expect(refusals, 'replace, delete and clear are refused while the pool loads the file').toEqual({
      replace: 'E_FILE_IN_USE',
      delete: 'E_FILE_IN_USE',
      clear: 'E_FILE_IN_USE',
    });
    expect(
      mid.state !== 3 && pMid.pages - p0.pages < 2 * planePages(q.frames),
      `the load had finished before the three refusals were answered (state ${mid.state}, ${pMid.pages - p0.pages} pages): the test proves nothing, use a longer fixture`,
    ).toBe(true);
    expect(await hashOf(path), 'the refused commands changed nothing').toBe(qHash);
    // an unrelated upload still works during the load (the store lock is shared with the load's card reads; the session retries E_BUSY)
    await dk.put(other, t.prep.stored);
    expect(await hashOf(other)).toBe(fileHashHex(t.prep.stored));
    expect(await hashOf(path), 'the leased file is unchanged after the other upload').toBe(qHash);

    const ready = await settle(0);
    expect(ready.state, `Q reaches ready ${JSON.stringify(ready.seen)}`).toBe(3);
    const r = await rec(0);
    expect(r.frames).toBe(q.frames);
    // the pool's copy is the old file, plane by plane
    for (const [channel, base] of [[0, r.baseL!], [1, r.baseR!]] as [number, number][]) {
      const got = await ddr(physOf(base), planePages(q.frames) * PAGE);
      expect(got.equals(plane(q, channel, LAYOUTS.wordrev!)), `plane ${channel} of the pool is the file that was loaded`).toBe(true);
    }
  });

  await phase('after the load', async () => {
    // the lease ended with the load: the same three commands work now
    await dk.put(path, t.prep.stored);
    expect(await hashOf(path), 'replace works after the load').toBe(fileHashHex(t.prep.stored));
    await dk.delete(path);
    expect(await hashOf(path), 'delete works after the load').toBeUndefined();
    await dk.delete(other);
    expect(await hashOf(other)).toBeUndefined();
  });
});
