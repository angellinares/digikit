import { REDRAW, rows, word, type Rt } from './fx-live.ts';
import { readTextRows, withPausedFxProtocol, shot } from './fx-session.ts';
import { type RuntimePlan } from './runtime-plan.ts';
import { symbolsOf } from './service-symbols.ts';

export const MASTER = 1;
export const SEND_ROUTE = 2;

/** The MODULES route panel's published diagnostics and its real front-panel navigation. */
export function routeUi(runtime: Rt, plan: RuntimePlan, artifacts: string, slots: number) {
  const { session } = runtime;
  // `fxp_send_count` and `DKRT_FXS` are the fx service file's (`symbolsOf`, bound after each boot by `inspectFxBoot`); the `DK_*` cells are the image's
  const sym = (name: string) => symbolsOf(runtime, plan)(name);
  const cell = (name: string) => word(session, sym(name));
  const bytes = (value: number, count: number) =>
    Array.from({ length: count }, (_, i) => (value >>> (24 - 8 * i)) & 0xff);
  const pending = async () => bytes(await cell('DK_FXR_PEND'), 4);
  const popup = async () => {
    const title = await readTextRows(session, sym('DK_POPUP'), 1, 48);
    const text = await readTextRows(session, sym('DK_POPUP_TEXT'), 1, 48);
    return [title[0], text[0]];
  };
  const toModules = async () => {
    await runtime.ui.tap('NO', { until: { ms: 400, why: REDRAW } });
    await runtime.ui.tap('SETTINGS', { until: { ms: 600, why: REDRAW } });
    for (let i = 0; i < 8; i++) await runtime.ui.tap('DOWN', { until: { ms: 200, why: REDRAW } });
    await runtime.ui.tap('YES', { until: { ms: 700, why: REDRAW } });
    for (let i = 0; i < 3; i++) await runtime.ui.tap('DOWN', { until: { ms: 200, why: REDRAW } });
    await runtime.ui.tap('YES', { until: { ms: 900, why: REDRAW } });
  };
  const goto = async (row: number) => {
    for (let i = 0; i < 5; i++) await runtime.ui.tap('UP', { until: { ms: 150, why: REDRAW } });
    for (let i = 0; i < row; i++) await runtime.ui.tap('DOWN', { until: { ms: 150, why: REDRAW } });
  };
  const panelRows = async () => readTextRows(session, sym('DK_PANEL'), await cell('DK_PANEL_N'), 24);
  const openPanel = async (row: number) => {
    await goto(row);
    await runtime.ui.tap('RIGHT', { until: { ms: 400, why: REDRAW } });
    return panelRows();
  };
  const routeChoice = async (row: number) => {
    await openPanel(row);
    await runtime.ui.tap('DOWN', { until: { ms: 250, why: REDRAW } });
    await runtime.ui.tap('YES', { until: { ms: 500, why: REDRAW } });
    const count = await cell('DK_ROUTE_N');
    return bytes(await cell('DK_ROUTE_ITEM'), count);
  };
  const pick = async (index: number) => {
    for (let i = 0; i < index; i++) await runtime.ui.tap('DOWN', { until: { ms: 250, why: REDRAW } });
    await runtime.ui.tap('YES', { until: { ms: 500, why: REDRAW } });
  };
  return {
    pending,
    popup,
    popups: () => cell('DK_POPUPS'),
    applies: () => cell('DK_APPLIES'),
    toModules,
    goto,
    panelRows,
    openPanel,
    routeChoice,
    pick,
    leaveMenu: async () => {
      for (let i = 0; i < 3; i++) await runtime.ui.tap('NO', { until: { ms: 400, why: REDRAW } });
    },
    routes: async (count: number) => (await rows(session, plan, slots)).slice(0, count).map((row) => row[18]),
    countCell: () => cell('fxp_send_count'),
    rowFlags: async (count: number) => {
      const base = sym('DKRT_FXS');
      return {
        allow: bytes(await word(session, base + 40), count),
        defaults: bytes(await word(session, base + 44), count),
      };
    },
    sendUsed: async () => (await withPausedFxProtocol(runtime, (dk) => dk.capacity())).SEND_SLOTS,
    fxPageOrder: async () => {
      // the service's copy of the table (`DKRT_FXP_TAB`): the image table has no state pointers
      const base = sym('DKRT_FXP_TAB');
      const first = await word(session, base + 4 * 5);
      const list = await word(session, base + 4 * 7);
      const count = await word(session, await word(session, base + 4 * 9));
      return Promise.all(Array.from({ length: count }, async (_, i) => (await word(session, list + 4 * i)) - first));
    },
    openSendFx: () => runtime.ui.tap('FX', { until: { ms: 400, why: REDRAW }, held: 'FUNC' }),
    pageDown: () => runtime.ui.tap('DOWN', { until: { ms: 300, why: REDRAW } }),
    shot: (tag: string) => shot(session, artifacts, tag),
  };
}
