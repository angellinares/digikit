import zlib from 'node:zlib';
import { CMD, LOG_KIND } from '@digi/dk-session/gen_proto';
import { expect, test } from 'vitest';
import {
  blob,
  cardInfo,
  claimHeader,
  bootInfo,
  E_CARD_RO,
  E_INUSE,
  E_NOSTORE,
  E_RANGE,
  probeWrite,
  pokeKnob,
  setSelection,
  statusOf,
} from '@digi/regression/runtime/card-kit.ts';
import { CARD_SYMBOLS, cardRigs, RT } from '@digi/regression/runtime/card-rig.ts';
import { CARD_TIMEOUT, NONE, openAbout, opStatuses } from '@digi/regression/runtime/debug-kit.ts';
import { cardStore, deviceProfile } from '@digi/regression/runtime/device-data.ts';
import { logEvents, ofKind } from '@digi/regression/runtime/runtime-menu.ts';

/**
 * Card safety (was Python `run_card`, `run_card_small`; deleted: 6a5bb1ed:tests/digikit/scenarios/card-safety.toml, 6a5bb1ed:tests/digikit/scenarios/card-small.toml): the one card-write function refuses writes
 * outside the store range; a blank card works as before; foreign bytes in the range make the card read-only for the runtime; a card with our store keeps
 * working; an upload on a blank card claims the range first, so a cut upload leaves a card the next boot calls ours; a card smaller than the store area has
 * no store. The write watch and the card writes are checked as `common_checks_upload`.
 * Also covers card-torn (was Python `run_card_torn`, deleted: c608bbf5:tests/digikit/scenarios/card-torn.toml): a power cut inside one sector write of the first
 * writes to a blank card.
 */

const MOD = 'modules/a.dkm';
const STRAY = 100; // relative sector of the foreign bytes
const PAT = Buffer.from(Array.from({ length: 512 }, (_, i) => (i * 7 + 3) & 0xff)); // neither all 0x00 nor all 0xFF
const BOOT_OK = { safe: 0, count: 0, uiReached: 1 };

cardRigs(
  'card safety',
  {
    devices: ['dt2'],
    dsp: 'zero',
    uploads: true,
    relaxFs: true, // the card after the cut upload carries the firmware's own writes: no filesystem check on the next boot
    roots: RT,
    symbols: [...CARD_SYMBOLS, 'DKRT_UPLOAD_CUT'],
  },
  (h) => {
    const { first, end } = cardStore(h.device);

    test(
      'a blank card works as before and the card-write function refuses every write outside the store range',
      async () => {
        const { session } = h.runtime;
        const check = async () => cardInfo(session, h.plan);
        expect(await check(), 'the check finds the range blank; nothing refused, claimed or written').toMatchObject({
          stateName: 'blank',
          firstNonblank: NONE,
          refused: 0,
          claims: 0,
          writes: 0,
        });
        const bad: [number, number][] = [
          [first - 1, 512],
          [end, 512],
          [end - 1, 1024],
          [first, 100],
          [0, 512],
          [first, 0],
          [0xffffffff, 512],
          [first - 1, 1024],
        ];
        const results: number[] = [];
        for (const [sector, n] of bad) results.push(await probeWrite(session, h.plan, sector, n));
        expect(results, 'every write outside the range is E_RANGE, whatever its size').toEqual(bad.map(() => E_RANGE));
        expect(await check(), 'the refusals are counted and nothing was written').toMatchObject({
          refused: bad.length,
          refusedRange: bad.length,
          writes: 0,
          stateName: 'blank',
        });

        expect(await setSelection(session, h.plan, [MOD])).toBe(0);
        const recs = await h.records();
        expect(recs[0], 'two record sectors are written, no store header').not.toBeNull();
        expect(recs[0]).toEqual(recs[1]);
        expect(await check(), 'the area is ours from then on').toMatchObject({
          stateName: 'ours',
          claims: 0,
          writes: 2,
        });

        const events = await logEvents(h.runtime, h.plan);
        const checks = ofKind(events, LOG_KIND.CARD_CHECK);
        const refused = ofKind(events, LOG_KIND.CARD_REFUSED);
        expect(checks, 'the card check (blank) is no error event').toHaveLength(1);
        expect(checks[0]!.args[0]).toBe(2);
        expect(checks[0]!.flags & 1).toBe(0);
        expect(refused, 'one event per refused write').toHaveLength(bad.length);
        for (const e of refused) {
          expect(e.flags & 1, 'an error event').toBe(1);
          expect(e.args[2], 'reason: outside the range').toBe(1);
        }
      },
      CARD_TIMEOUT,
    );

    test(
      'a selection record only: blank header sectors give an empty store, foreign bytes in both headers give no store',
      async () => {
        const headerB = deviceProfile(h.device).card.store.header_b_sector;
        // a valid selection record and nothing else in the range makes the area ours
        let rt = await h.boot(await h.card({ selection: [] }));
        await h.ready();
        expect((await cardInfo(rt.session, h.plan)).stateName).toBe('ours');
        expect(await statusOf(() => rt.dk.put('modules/s.dkm', blob(3_000, 4))), 'an upload works').toBe(0);
        expect((await rt.dk.list()).map((e) => e.path)).toContain('modules/s.dkm');

        // foreign bytes in header A and B: the store is somebody's data
        rt = await h.boot(await h.card({ selection: [], foreign: { [first]: PAT, [headerB]: PAT } }));
        await h.ready();
        const ci = await cardInfo(rt.session, h.plan);
        expect(ci.stateName, 'the card counts as ours').toBe('ours');
        expect(await opStatuses(rt.dk, ['begin', 'delete', 'list'])).toEqual([E_NOSTORE, E_NOSTORE, E_NOSTORE]);
        await rt.dk.request(CMD.DATA, Buffer.alloc(14)); // DATA with no upload open: E_STATE, nothing staged
        expect(await h.storeWrites(), 'nothing was written to the store range').toEqual([]);
        expect((await cardInfo(rt.session, h.plan)).writes).toBe(0);
      },
      CARD_TIMEOUT,
    );

    test(
      'a card with our store keeps working: the selection loads, a selection and an upload still work',
      async () => {
        const rt = await h.boot(await h.card({ modules: [h.mod('test/c-demo', 'a')], selection: [MOD] }));
        await h.ready();
        expect(await cardInfo(rt.session, h.plan), 'the check says ours without reading the range').toMatchObject({
          stateName: 'ours',
          firstNonblank: NONE,
          hash: 0,
          refused: 0,
        });
        expect(await bootInfo(rt.session, h.plan)).toMatchObject({ count: 1, loaded: 1, safe: 0 });

        expect(await setSelection(rt.session, h.plan, [MOD, MOD])).toBe(0);
        expect(await statusOf(() => rt.dk.put('modules/b.dkm', blob(70_000, 5)))).toBe(0);
        expect((await rt.dk.list()).map((e) => e.path)).toContain('modules/b.dkm');
        expect(await cardInfo(rt.session, h.plan), 'the range check refused nothing of theirs').toMatchObject({
          refused: 0,
          claims: 0,
        });
      },
      CARD_TIMEOUT,
    );

    test(
      'an upload on a blank card claims the range first: after a cut upload the next boot calls the area ours',
      async () => {
        const rt = await h.boot(await h.card());
        await h.ready();
        await pokeKnob(rt.session, h.plan, 'DKRT_UPLOAD_CUT', 1);
        // three DATA chunks: the second write is refused
        expect(await statusOf(() => rt.dk.put('modules/c.dkm', blob(70_000, 9))), 'the cut upload failed').not.toBe(0);
        expect(await cardInfo(rt.session, h.plan), 'the first data write claimed the range').toMatchObject({
          claims: 1,
          stateName: 'ours',
        });

        const next = await h.boot(await h.after());
        await h.ready();
        expect((await cardInfo(next.session, h.plan)).stateName, 'ours, not foreign').toBe('ours');
        expect(await next.dk.list(), 'the store is empty').toEqual([]);
        expect(await statusOf(() => next.dk.put('modules/c.dkm', blob(5_000, 3))), 'an upload works').toBe(0);
      },
      CARD_TIMEOUT,
    );
  },
);

cardRigs(
  'card safety: foreign bytes',
  {
    devices: ['dt2'],
    uploads: true,
    roots: [...RT, 'core/digikit-menu'],
    symbols: [...CARD_SYMBOLS, 'DK_ABOUT', 'DK_ABOUT_N'],
  },
  (h) => {
    test(
      'foreign bytes in the range make the card read-only: nothing written, no selection, no upload, ABOUT says so',
      async () => {
        const { first } = cardStore(h.device);
        const rt = await h.boot(await h.card({ foreign: { [first + STRAY]: PAT } }));
        await h.ready();
        const { session, dk } = rt;
        const hash = zlib.crc32(PAT);
        expect(
          await cardInfo(session, h.plan),
          'the check names the first non-blank sector and its crc32',
        ).toMatchObject({
          stateName: 'foreign',
          firstNonblank: STRAY,
          hash,
        });
        expect(await bootInfo(session, h.plan), 'the boot runs normally').toMatchObject(BOOT_OK);

        const rSel = await setSelection(session, h.plan, [MOD]);
        const rProbe = await probeWrite(session, h.plan, first + 5, 512);
        const rRange = await probeWrite(session, h.plan, first - 1, 512);
        expect([rSel, rProbe, rRange], 'in the range E_CARD_RO, outside it E_RANGE').toEqual([
          E_CARD_RO,
          E_CARD_RO,
          E_RANGE,
        ]);
        expect(await cardInfo(session, h.plan)).toMatchObject({ refused: 3, writes: 0 });
        expect(await h.records(), 'no selection record was written').toEqual([null, null]);

        expect((await dk.info()).card_read_only, 'INFO says the card is read-only').toBe(true);
        expect(await dk.list(), 'LIST works (empty)').toEqual([]);
        expect(await opStatuses(dk, ['begin', 'delete', 'clear'])).toEqual([E_INUSE, E_INUSE, E_INUSE]);

        const checks = ofKind(await logEvents(rt, h.plan), LOG_KIND.CARD_CHECK);
        expect(checks, 'the log reports what the check found').toHaveLength(1);
        expect(checks[0]!.flags & 1, 'an error event').toBe(1);
        expect(checks[0]!.args.slice(0, 3)).toEqual([3, STRAY, hash]);

        const rows = await openAbout(rt, h.plan);
        expect(rows).toEqual(expect.arrayContaining(['CARD READ-ONLY', `NOT OURS AT ${STRAY}`, 'NO BOOT COUNTER']));
        for (const row of rows) expect(row.length, row).toBeLessThanOrEqual(21);
        expect(await h.storeWrites(), 'the runtime wrote nothing in the store range').toEqual([]);
      },
      CARD_TIMEOUT,
    );
  },
);

cardRigs('card safety: torn sectors', { dsp: 'zero', uploads: true, roots: RT, symbols: CARD_SYMBOLS }, (h) => {
  const { first, selA } = cardStore(h.device);
  const headerB = deviceProfile(h.device).card.store.header_b_sector;
  const claim = claimHeader(h.device);
  const torn = Buffer.concat([claim.subarray(0, 256), Buffer.alloc(256)]); // the cut came after half the sector

  test(
    "header B torn by the claim's first write, header A blank: the card reads blank, an upload answers E_NOSTORE and writes nothing; CLEAR recovers it",
    async () => {
      const rt = await h.boot(await h.card({ foreign: { [headerB]: torn } }));
      await h.ready();
      expect((await cardInfo(rt.session, h.plan)).stateName).toBe('blank');
      expect(await statusOf(() => rt.dk.put('modules/b.dkm', blob(3_000, 31)))).toBe(E_NOSTORE);
      expect(await h.storeWrites(), 'nothing written in the store range').toEqual([]);
      expect(await statusOf(() => rt.dk.clear()), 'CLEAR').toBe(0);
      expect(await statusOf(() => rt.dk.put('modules/b.dkm', blob(3_000, 31))), 'the next upload commits').toBe(0);
      expect((await rt.dk.list()).map((e) => e.path)).toContain('modules/b.dkm');
    },
    CARD_TIMEOUT,
  );

  test(
    "header A torn by the claim's second write, header B whole: the card is ours and an upload commits",
    async () => {
      const rt = await h.boot(await h.card({ foreign: { [first]: torn, [headerB]: claim } }));
      await h.ready();
      expect((await cardInfo(rt.session, h.plan)).stateName).toBe('ours');
      expect(await statusOf(() => rt.dk.put('modules/a.dkm', blob(3_000, 31)))).toBe(0);
      expect((await rt.dk.list()).map((e) => e.path)).toContain('modules/a.dkm');
    },
    CARD_TIMEOUT,
  );

  test(
    'header A torn and header B blank (claim writes out of order): the card is foreign at sector 0, BEGIN and CLEAR answer E_INUSE, nothing is written',
    async () => {
      const rt = await h.boot(await h.card({ foreign: { [first]: torn } }));
      await h.ready();
      expect(await cardInfo(rt.session, h.plan)).toMatchObject({ stateName: 'foreign', firstNonblank: 0 });
      expect(await opStatuses(rt.dk, ['begin', 'clear'])).toEqual([E_INUSE, E_INUSE]);
      expect(await h.storeWrites()).toEqual([]);
    },
    CARD_TIMEOUT,
  );

  test(
    'selection copy A torn by the first selection write, the rest blank: the card is blank and the next selection writes both copies',
    async () => {
      const tornSel = Buffer.concat([PAT.subarray(0, 256), Buffer.alloc(256)]);
      const rt = await h.boot(await h.card({ foreign: { [selA]: tornSel } }));
      await h.ready();
      expect((await cardInfo(rt.session, h.plan)).stateName).toBe('blank');
      expect(await setSelection(rt.session, h.plan, [MOD])).toBe(0);
      const recs = await h.records();
      expect(recs[0]).not.toBeNull();
      expect(recs[0]).toEqual(recs[1]);
    },
    CARD_TIMEOUT,
  );
});

// A card smaller than the store area (the emulator's `small` identity reports 0x3B0000 sectors, below the area of both devices): no store.
cardRigs(
  'card-small',
  {
    initial: 'small',
    relaxFs: true,
    env: { DIGI_CARD_IDENTITY: 'small' },
    uploads: true,
    roots: RT,
    symbols: CARD_SYMBOLS,
  },
  (h) => {
    test(
      'a card smaller than the store area has no store: nothing is read or written there and uploads answer E_NOSTORE',
      async () => {
        await h.ready();
        const { session, dk } = h.runtime;
        expect(
          await cardInfo(session, h.plan),
          'the check says too small; nothing refused, claimed or written',
        ).toMatchObject({
          stateName: 'too small',
          refused: 0,
          claims: 0,
          writes: 0,
        });
        expect(await bootInfo(session, h.plan), 'the boot goes on').toMatchObject(BOOT_OK);

        const checks = ofKind(await logEvents(h.runtime, h.plan), LOG_KIND.CARD_CHECK);
        expect(checks, 'one error event for the check (state too small)').toHaveLength(1);
        expect(checks[0]!.flags & 1).toBe(1);
        expect(checks[0]!.args[0]).toBe(5);

        await dk.info();
        expect(
          await opStatuses(dk, ['begin', 'delete', 'mkdir', 'clear', 'list']),
          'every store command is E_NOSTORE',
        ).toEqual(Array(5).fill(E_NOSTORE));
        const { STORE } = await dk.capacity();
        expect(STORE).toMatchObject({ limit: 0, used: 0 });

        const rSel = await setSelection(session, h.plan, [MOD]);
        const rProbe = await probeWrite(session, h.plan, cardStore(h.device).first + 5, 512);
        expect([rSel, rProbe], 'a selection and a write in the area are refused').toEqual([E_CARD_RO, E_CARD_RO]);
        expect((await cardInfo(session, h.plan)).writes).toBe(0);
        // the stock firmware formats this blank card (writes below the store area), so only the store range is checked
        expect(await h.storeWrites(), 'the runtime wrote nothing in the store range').toEqual([]);
      },
      CARD_TIMEOUT,
    );
  },
);
