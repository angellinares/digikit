import { be32 } from '@digi/dk-session';
import { button, type Device } from '@digi/test-support';
import { expect, test } from 'vitest';
import { bootInfo, bootInfoLoaded, CMD as MAILBOX, currentRecord, encodeRecord, postRaw } from '@digi/regression/runtime/card-kit.ts';
import { CARD_SYMBOLS, cardRigs, RT } from '@digi/regression/runtime/card-rig.ts';
import { CARD_TIMEOUT, settleHang } from '@digi/regression/runtime/debug-kit.ts';
import { rpc } from '@digi/regression/runtime/fx-live.ts';
import { drainInputs } from '@digi/regression/runtime/fx-session.ts';
import { introNow, safeFrame } from '@digi/regression/runtime/intro-frame.ts';
import { planSymbol } from '@digi/regression/runtime/runtime-plan.ts';
import { waitEmulatedFrames } from '@digi/regression/runtime/waits.ts';

/**
 * Safe mode and power cuts (the Python scenarios, deleted: safe-fault, safe-late-fault and cut-selection, all full
 * tier): the boot counter, the safe-once flag, a stable period that clears the counter, and the card states a power cut can leave between the two
 * selection record copies. A boot is one card image; the next boot runs on the card the last one left.
 */

type BootInfo = Awaited<ReturnType<typeof bootInfo>>;
const E_SAFE_MODE = -44;
const SAFE_COUNTER = 1;
const SAFE_KEY = 2;
const SAFE_FLAG = 3;
/** Scenario safe-fault: the module that follows `fault` on the card of the safe-once flag. */
const FLAG_MODULES: Record<Device, string[]> = { dt2: ['test/sine'], dn2: ['test/dyn-dn2'] };
/** `cut-selection.toml`. */
const CUT_MODULES: Record<Device, string[]> = { dt2: ['test/sine'], dn2: ['test/dyn-dn2', 'test/sine'] };
/** Scenario safe-late-fault: the stable period is shortened to 9,000 blocks; the module hangs the UI task 8,000 or 10,000 blocks in. */
const LATE = { stable: 9000, early: 8000, late: 10000 };
/** `cut-selection.toml` cases: the copies A and B, and what the boot uses. */
const CUT_CASES = [
  { a: 'new', b: 'old', want: { loaded: 'all', seq: 2, copy: 0 } },
  { a: 'torn', b: 'old', want: { loaded: 0, seq: 1, copy: 1 } },
  { a: 'old', b: 'new', want: { loaded: 'all', seq: 2, copy: 1 } },
  { a: 'torn', b: 'torn', want: { loaded: 0, seq: 0, copy: 0 } },
] as const;


cardRigs('safe mode', { dsp: 'sharc', full: true, relaxFs: true, roots: RT, symbols: CARD_SYMBOLS }, (h) => {
  test(
    'a module that resets the machine in its init: two boots never reach the UI loop, the third boots in safe mode',
    async () => {
      const fault = h.mod('test/dyn-fault', 'fault');
      // Three boots of the fault card run inside this one reload; the hang guard covers all three.
      await h.boot(await h.card({ modules: [fault], selection: [fault.name] }), { timeoutMs: 600_000 });
      const info = await h.ready(true);
      expect(info, 'the third boot is safe mode (counter 2)').toMatchObject({
        safe: 1,
        reason: SAFE_COUNTER,
        counter: 2,
      });
      expect(info.loaded, 'safe mode loaded no module').toBe(0);
      expect(info.allResults[0], 'and reported it').toBe(E_SAFE_MODE);
      expect((await introNow(h.session)).equals(safeFrame('counter')), 'the intro shows the 2 FAILED BOOTS frame').toBe(true);
      const rec = currentRecord(await h.records());
      expect(rec?.counter, 'the UI loop ran: counter 0').toBe(0);
      expect(rec?.paths, 'selection kept').toEqual([fault.name]);
    },
    CARD_TIMEOUT,
  );

  test(
    'the safe-once flag gives safe mode (flag), no module, and the flag is cleared',
    async () => {
      const others = FLAG_MODULES[h.device].map((id, i) => h.mod(id, `m${i}`));
      await h.boot(await h.card({ modules: others, selection: others.map((m) => m.name), flags: 1 }));
      const info = await h.ready(true);
      expect(info).toMatchObject({ safe: 1, reason: SAFE_FLAG, loaded: 0 });
      const rec = currentRecord(await h.records());
      expect(rec?.flags, 'flag cleared').toBe(0);
      expect(rec?.paths, 'selection kept').toEqual(others.map((m) => m.name));
    },
    CARD_TIMEOUT,
  );

  for (const kind of ['early', 'late'] as const) {
    test(
      `a module that hangs the UI task ${kind} in a session: the boot counter and the stable period`,
      async () => {
        const mod = h.mod('test/dyn-fault-late', 'late');
        const after = h.cfg('fault-after.bin', Buffer.concat([be32(LATE[kind]), Buffer.from([1])])); // hang
        let card = await h.card({ modules: [mod, after], selection: [mod.name], stableBlocks: LATE.stable });
        const ticks = planSymbol(h.plan, 'DKRT_STATE') + 8; // the runtime's UI-loop count
        const seen: { info: BootInfo; after: BootInfo; rec: ReturnType<typeof currentRecord>; hung: boolean }[] = [];
        for (let boot = 0; boot < 3; boot++) {
          const { session } = await h.boot(card);
          const first = await bootInfo(session, h.plan);
          await rpc(session, 'run.mode', { mode: 'paused' });
          const hung = await settleHang(session, ticks);
          seen.push({
            info: first,
            after: await bootInfo(session, h.plan),
            rec: currentRecord(await h.records()),
            hung,
          });
          if (boot < 2) card = await h.after(); // a power cycle: the card as the last boot left it
        }
        const summary = JSON.stringify(seen.map((s) => [s.info.counter, s.after.stable, s.rec?.counter, s.hung]));
        const counters = seen.map((s) => s.info.counter);
        if (kind === 'early') {
          for (const s of seen.slice(0, 2)) {
            expect(s.info, `boots 1 and 2 load the module ${summary}`).toMatchObject({ safe: 0, loaded: 1 });
            expect(s.hung, 'and the UI task hangs in it').toBe(true);
            expect(s.after.stable, 'the failure came before the stable period').toBe(0);
            expect(s.rec?.counter, 'the counter is raised at the start of each boot').toBe(s.info.counter + 1);
          }
          expect(counters, 'not cleared').toEqual([0, 1, 2]);
          const third = seen[2]!;
          expect(third.info, 'two bad boots in a row: safe mode (counter), no module').toMatchObject({
            safe: 1,
            reason: SAFE_COUNTER,
            loaded: 0,
          });
          expect(third.hung, 'the UI keeps running').toBe(false);
          expect(third.after.stable, 'the safe boot clears the counter at its first UI-loop pass').toBe(1);
          expect(third.rec?.counter).toBe(0);
          expect(third.rec?.paths, 'selection kept').toEqual([mod.name]);
        } else {
          for (const s of seen) {
            expect(s.info, `every boot loads the module ${summary}`).toMatchObject({ safe: 0, loaded: 1 });
            expect(s.hung, 'and the UI task hangs in it').toBe(true);
            expect(s.after.stable, 'the stable period was over before the failure').toBe(1);
            expect(s.rec?.counter, 'the counter was cleared').toBe(0);
          }
          expect(counters, 'every boot starts at 0, no boot is safe').toEqual([0, 0, 0]);
        }
      },
      CARD_TIMEOUT,
    );
  }
});

// REC (the safe-mode key) is queued before the cold boot: held, or pressed and released.
for (const held of [true, false]) {
  cardRigs(
    `cold boot with REC ${held ? 'held' : 'released'}`,
    { dsp: 'sharc',
      roots: RT, // default tier: these two cases run in the normal gate
      symbols: CARD_SYMBOLS,
      startEmpty: true,
      initial: () => ({ modules: ['test/c-demo'], selection: ['modules/c-demo.dkm'] }),
      initialButtons: held
        ? [{ code: button('RECORD'), down: true }]
        : [
            { code: button('RECORD'), down: true },
            { code: button('RECORD'), down: false },
          ],
    },
    (h) => {
      test(
        held ? 'REC held: safe mode (key), no module loaded' : 'REC released: no safe mode, the module loads',
        async () => {
          await expect.poll(async () => (await bootInfo(h.session, h.plan)).uiReached, { timeout: 30_000 }).toBe(1);
          // Check the first decision, not a later boot: a wrong safe result must fail here.
          const first = await bootInfo(h.session, h.plan);
          expect(first).toMatchObject({ safe: held ? 1 : 0, reason: held ? SAFE_KEY : 0, count: 1 });
          // the safe-mode screen replaces the stock intro in a safe boot only (its bytes are the host test's fixture)
          const shown = await introNow(h.session);
          expect(shown.equals(safeFrame('key')), held ? 'REC held: the SAFE MODE / REC HELD frame' : 'REC released: not the safe-mode frame').toBe(held);
          expect(shown.equals(safeFrame('counter')), 'never the counter frame').toBe(false);
          const info = held ? first : await bootInfoLoaded(h.session, h.plan);
          expect(info).toMatchObject({ loaded: held ? 0 : 1 });
          await rpc(h.session, 'run.mode', { mode: 'free' });
          try {
            if (held) await rpc(h.session, 'input.button', { code: button('RECORD'), down: false });
            await waitEmulatedFrames(h.session, 150);
          } finally {
            try {
              await drainInputs(h.session);
            } finally {
              await rpc(h.session, 'run.mode', { mode: 'paused' });
            }
          }
        },
        CARD_TIMEOUT,
      );
    },
  );
}

cardRigs(
  'power cut between the selection record copies',
  { dsp: 'sharc', full: true, roots: RT, symbols: CARD_SYMBOLS },
  (h) => {
    const modules = () => CUT_MODULES[h.device].map((id, i) => h.mod(id, `m${i}`));

    for (const { a, b, want } of CUT_CASES) {
      test(
        `power cut, A ${a} / B ${b}: the boot uses the valid copy with the higher sequence number`,
        async () => {
          const mods = modules();
          const paths = mods.map((m) => m.name);
          const fresh = encodeRecord({ paths, seq: 2 });
          const torn = Buffer.from(fresh);
          torn[100] = torn[100]! ^ 0xff;
          const named = { new: fresh, old: encodeRecord({ paths: [], seq: 1 }), torn };
          const { session } = await h.boot(await h.card({ modules: mods, recs: [named[a], named[b]] }));
          await h.ready();
          const info = await bootInfoLoaded(session, h.plan);
          expect(info, `loaded ${info.loaded} of ${info.count} (results ${info.results.join(",")}) seq ${info.seq} copy ${info.copy}`).toMatchObject({
            loaded: want.loaded === 'all' ? mods.length : want.loaded,
            seq: want.seq,
            copy: want.copy,
            safe: 0,
          });
        },
        CARD_TIMEOUT,
      );
    }

    test(
      'one copy written by the runtime: A new, B still empty, and the restart after the cut loads the new copy',
      async () => {
        const mods = modules();
        const { session, dk } = await h.boot(await h.card({ modules: mods }));
        await h.ready();
        const r = await postRaw(
          session,
          h.plan,
          MAILBOX.SELECT_ONE,
          Buffer.from(`\0${mods.map((m) => m.name).join('\0')}\0\0`),
        );
        const recs = await h.records();
        expect(r).toBe(0);
        expect(recs[0]?.seq, 'A is new').toBe(1);
        expect(recs[1], 'B is still empty').toBeNull();

        await dk.restart();
        await dk.waitForBoot();
        await h.ready();
        expect(await bootInfoLoaded(session, h.plan), 'the restart loads the new copy').toMatchObject({
          loaded: mods.length,
          copy: 0,
        });
      },
      CARD_TIMEOUT,
    );
  },
);
