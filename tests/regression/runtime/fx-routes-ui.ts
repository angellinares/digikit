import type { PatchedSession } from '../support.ts';
import { rows, word } from './fx-live.ts';
import { readTextRows, withPausedFxProtocol, shot, tap } from './fx-session.ts';
import { planSymbol, type RuntimePlan } from './runtime-plan.ts';

export const MASTER = 1;
export const SEND_ROUTE = 2;

/** The MODULES route panel's published diagnostics and its real front-panel navigation. */
export function routeUi(runtime: PatchedSession, plan: RuntimePlan, artifacts: string, slots: number) {
  const { session } = runtime;
  const sym = (name: string) => planSymbol(plan, name);
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
    await tap(session, 'NO', 400);
    await tap(session, 'SETTINGS', 600);
    for (let i = 0; i < 8; i++) await tap(session, 'DOWN', 200);
    await tap(session, 'YES', 700);
    for (let i = 0; i < 3; i++) await tap(session, 'DOWN', 200);
    await tap(session, 'YES', 900);
  };
  const goto = async (row: number) => {
    for (let i = 0; i < 5; i++) await tap(session, 'UP', 150);
    for (let i = 0; i < row; i++) await tap(session, 'DOWN', 150);
  };
  const panelRows = async () => readTextRows(session, sym('DK_PANEL'), await cell('DK_PANEL_N'), 24);
  const openPanel = async (row: number) => {
    await goto(row);
    await tap(session, 'RIGHT', 400);
    return panelRows();
  };
  const routeChoice = async (row: number) => {
    await openPanel(row);
    await tap(session, 'DOWN', 250);
    await tap(session, 'YES', 500);
    const count = await cell('DK_ROUTE_N');
    return bytes(await cell('DK_ROUTE_ITEM'), count);
  };
  const pick = async (index: number) => {
    for (let i = 0; i < index; i++) await tap(session, 'DOWN', 250);
    await tap(session, 'YES', 500);
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
      for (let i = 0; i < 3; i++) await tap(session, 'NO', 400);
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
      const base = sym('fxpages_info');
      const first = await word(session, base + 4 * 5);
      const list = await word(session, base + 4 * 7);
      const count = await word(session, await word(session, base + 4 * 9));
      return Promise.all(Array.from({ length: count }, async (_, i) => (await word(session, list + 4 * i)) - first));
    },
    openSendFx: () => tap(session, 'FX', 400, 'FUNC'),
    pageDown: () => tap(session, 'DOWN', 300),
    shot: (tag: string) => shot(session, artifacts, tag),
  };
}
