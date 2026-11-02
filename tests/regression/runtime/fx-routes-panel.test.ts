import { describe, expect, test } from 'vitest';
import { DEVICES, FIRMWARE, record } from './live.ts';
import { GAIN, SINE } from './fixtures.ts';
import { withRouteFxSession } from './fx-route-live.ts';
import { inspectFxBoot, tap } from './fx-session.ts';
import { MASTER, routeUi, SEND_ROUTE } from './fx-routes-ui.ts';

const SEND = 'test/dyn-fx-gain-send';
const SENDONLY = 'test/dyn-fx-gain-sendonly';
const SEND3 = `${SEND}-3`;
const selection = (...indices: number[]) => indices.map((index) => `modules/m${index}.dkm`);

for (const device of DEVICES) {
  describe(`${device.toUpperCase()} ${FIRMWARE[device]} FX route panel`, () => {
    const context = withRouteFxSession(device, 'FX routes panel', {
      image: 'routes',
      initial: 'none',
      requireAudible: false,
      presets: {
        none: { selection: [] },
        list: { selection: selection(0, 1, 2) },
        sendonly: { selection: selection(3, 1) },
        fallback: {
          selection: selection(1, 3),
          settings: [
            [`fx/route/${GAIN}`, '01'],
            [`fx/route/${SENDONLY}`, '00'],
          ],
        },
        full: {
          selection: device === 'dt2' ? selection(2, 3, 4) : selection(2, 4),
          settings: [[`fx/route/${SEND3}`, '00']],
        },
      },
    });

    test('preserves manifest routes, pending choices, cancellation and send-slot capacity across clean cases', async () => {
      const { fixture, plan, artifacts, slots, sendSlots } = context;
      let runtime = fixture.runtime;
      const inspect = async (tag: string, names: string[]) => {
        await inspectFxBoot(runtime, plan, names, artifacts, tag);
        context.rememberEpoch(runtime.epoch);
        return routeUi(runtime, plan, artifacts, slots);
      };
      const reload = async (tag: string) => {
        const previous = runtime;
        runtime = await fixture.reload(context.baseline(tag));
        expect(runtime.session).toBe(previous.session);
        expect(runtime.epoch).toBeGreaterThan(previous.epoch);
        await expect(previous.dk.info()).rejects.toThrow();
      };

      let ui = await inspect('none', []);
      await ui.toModules();
      const unloadedPanels = [];
      for (const row of [0, 1, 2]) unloadedPanels.push(await ui.openPanel(row));
      await ui.shot('step4-none');
      expect(unloadedPanels, 'unloaded modules have no ROUTE panel row').toEqual(Array(3).fill(['LOAD', 'INFO']));

      await reload('list');
      ui = await inspect('list', [SINE, GAIN, SEND]);
      expect(await ui.rowFlags(2), 'manifest allowed routes and defaults').toEqual({
        allow: [MASTER, MASTER | SEND_ROUTE],
        defaults: [MASTER, SEND_ROUTE],
      });
      expect(await ui.routes(2)).toEqual([0, 1]);
      expect(await ui.countCell()).toBe(2);
      expect(await ui.sendUsed()).toEqual({ limit: sendSlots, used: 1, largest: 0 });
      const order = await ui.fxPageOrder();
      expect(order).toHaveLength(5);
      expect(order.slice(3), 'send page precedes master page after three stock pages').toEqual([1, 0]);
      record(artifacts, 'list-fx-page-order', order);
      await ui.openSendFx();
      await ui.shot('step4-fxview-1');
      for (let page = 2; page <= 5; page++) {
        await ui.pageDown();
        await ui.shot(`step4-fxview-${page}`);
      }
      await ui.leaveMenu();
      await ui.toModules();
      expect(await ui.openPanel(0)).toEqual(['UNLOAD', 'INFO']);
      await tap(runtime.session, 'NO', 300);
      expect(await ui.openPanel(1)).toEqual(['UNLOAD', 'ROUTE...', 'INFO']);
      await tap(runtime.session, 'NO', 300);
      expect((await ui.pending()).slice(0, 2)).toEqual([MASTER, SEND_ROUTE]);
      const popups = await ui.popups();
      expect(await ui.routeChoice(1)).toEqual([MASTER]);
      await ui.shot('step4-master-only');
      await ui.pick(0);
      await tap(runtime.session, 'NO', 500);
      expect((await ui.pending()).slice(0, 2)).toEqual([MASTER, SEND_ROUTE]);
      expect(await ui.popups()).toBe(popups);
      await tap(runtime.session, 'YES', 900);
      expect(await ui.routeChoice(2)).toEqual([SEND_ROUTE, MASTER]);
      await ui.shot('step4-route-choice');
      await ui.pick(1);
      expect((await ui.pending()).slice(0, 2)).toEqual([MASTER, MASTER]);
      expect(await ui.applies()).toBe(0);
      await tap(runtime.session, 'NO', 700);
      await ui.shot('step4-apply-question');
      expect(await ui.popups()).toBe(popups + 1);
      expect(await ui.popup()).toEqual(['APPLY & RESTART?', '1 CHANGE']);
      await tap(runtime.session, 'NO', 700);
      expect((await ui.pending()).slice(0, 2)).toEqual([MASTER, SEND_ROUTE]);
      await tap(runtime.session, 'YES', 900);
      await ui.routeChoice(2);
      await ui.pick(1);
      await ui.routeChoice(2);
      await ui.pick(0);
      await tap(runtime.session, 'NO', 500);
      expect((await ui.pending()).slice(0, 2)).toEqual([MASTER, SEND_ROUTE]);
      expect(await ui.popups()).toBe(popups + 1);

      await reload('sendonly');
      ui = await inspect('sendonly', [SENDONLY, GAIN]);
      expect(await ui.rowFlags(2)).toEqual({ allow: [SEND_ROUTE, MASTER], defaults: [SEND_ROUTE, MASTER] });
      await ui.toModules();
      expect((await ui.pending()).slice(0, 2)).toEqual([SEND_ROUTE, MASTER]);
      const sendOnlyPopups = await ui.popups();
      expect(await ui.routeChoice(3)).toEqual([SEND_ROUTE]);
      await ui.shot('step4-send-only');
      await ui.pick(0);
      await tap(runtime.session, 'NO', 500);
      expect((await ui.pending()).slice(0, 2)).toEqual([SEND_ROUTE, MASTER]);
      expect(await ui.popups()).toBe(sendOnlyPopups);

      await reload('fallback');
      ui = await inspect('fallback', [GAIN, SENDONLY]);
      expect(await ui.routes(2), 'disallowed stored routes fall back to manifest defaults').toEqual([0, 1]);
      expect((await ui.sendUsed())!.used).toBe(1);
      await ui.toModules();
      expect((await ui.pending()).slice(0, 2)).toEqual([MASTER, SEND_ROUTE]);

      await reload('full');
      const fullNames = sendSlots > 1 ? [SEND, SENDONLY, SEND3] : [SEND, SEND3];
      ui = await inspect('full', fullNames);
      const fullPending = [...Array<number>(sendSlots).fill(SEND_ROUTE), MASTER];
      expect(await ui.routes(fullNames.length)).toEqual([...Array<number>(sendSlots).fill(1), 0]);
      expect(await ui.sendUsed()).toEqual({ limit: sendSlots, used: sendSlots, largest: 0 });
      await ui.toModules();
      expect((await ui.pending()).slice(0, fullNames.length)).toEqual(fullPending);
      expect(await ui.routeChoice(4)).toEqual([MASTER]);
      await ui.shot('step4-no-send-slot');
      await ui.pick(0);
      expect((await ui.pending()).slice(0, fullNames.length)).toEqual(fullPending);
      expect(await ui.routeChoice(2)).toEqual([SEND_ROUTE, MASTER]);
      await ui.pick(1);
      expect(await ui.routeChoice(4), 'pending move to master frees a send slot').toEqual([SEND_ROUTE, MASTER]);
      await ui.pick(0);
      await ui.shot('step4-pending-counts');
      expect((await ui.pending()).slice(0, fullNames.length)).toEqual([
        MASTER,
        ...fullPending.slice(1, -1),
        SEND_ROUTE,
      ]);
      await tap(runtime.session, 'NO', 700);
      expect(await ui.popup()).toEqual(['APPLY & RESTART?', '2 CHANGES']);
    });
  });
}
