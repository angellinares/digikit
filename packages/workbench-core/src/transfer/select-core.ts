// The "select all" checkbox of a pane header (pure, no DOM; apps/desktop/ui/tests/transfer-page-ui.test.ts): its three states over the rows
// that are visible, and what a click does. Ticks of rows that are not visible (another folder, the test modules hidden) are left alone.

export type SelectState = 'none' | 'some' | 'all';

/** None, some or all of `keys` (the visible rows that can be ticked) are in `ticked`. No rows: none. */
export function selectState(keys: readonly string[], ticked: ReadonlySet<string>): SelectState {
  const n = keys.filter((k) => ticked.has(k)).length;
  return n === 0 ? 'none' : n === keys.length ? 'all' : 'some';
}

/** The ticks after a click on the header checkbox: all visible rows ticked unless they all are already (then none of them). */
export function toggleAll(keys: readonly string[], ticked: ReadonlySet<string>): Set<string> {
  const next = new Set(ticked);
  if (selectState(keys, ticked) === 'all') for (const k of keys) next.delete(k);
  else for (const k of keys) next.add(k);
  return next;
}

// ---- the selection of a list, a checklist ----------------------------------------------------------------------------------------------
// The tick of a row is its selection: one set per pane, changed by a click (toggle one row), Shift (add a range from the anchor), Cmd or Ctrl
// (toggle one row), a drag over empty space (the marquee), the keys, and the header box. `keys` are the rows in list order (the rows that can be selected).

/** The result of a click or a key: the new selection and the row that Shift extends from. */
export interface Selection {
  selected: Set<string>;
  anchor: string | undefined;
}

export interface Mods {
  shift: boolean;
  /** Cmd on macOS, Ctrl on Windows and Linux. */
  mod: boolean;
}

/** The modifiers of a mouse or key event (Cmd and Ctrl count the same everywhere). */
export const modsOf = (e: { shiftKey: boolean; metaKey: boolean; ctrlKey: boolean }): Mods => ({
  shift: e.shiftKey,
  mod: e.metaKey || e.ctrlKey,
});

/** The keys from `a` to `b` in list order, both included (either order). An unknown end gives just the other one. */
export function rangeKeys(keys: readonly string[], a: string | undefined, b: string): string[] {
  const j = keys.indexOf(b);
  const i = a === undefined ? j : keys.indexOf(a);
  if (j < 0) return [];
  if (i < 0) return [b];
  return keys.slice(Math.min(i, j), Math.max(i, j) + 1);
}

/**
 * A click on a row. The panes are checklists: a plain click (or Cmd or Ctrl) toggles this row, like its checkbox, and keeps the other ticks.
 * Shift: the range from the anchor to this row is added to the selection. The anchor moves with a toggle and stays for Shift.
 */
export function clickSelect(
  keys: readonly string[],
  selected: ReadonlySet<string>,
  anchor: string | undefined,
  key: string,
  mods: Mods,
): Selection {
  if (mods.shift) {
    const from = anchor !== undefined && keys.includes(anchor) ? anchor : key;
    const range = rangeKeys(keys, from, key);
    return { selected: new Set([...selected, ...range]), anchor: from };
  }
  return tickSelect(selected, key);
}

/** The tick of one row (the checkbox): it toggles that row only. A plain click on the row does the same. */
export function tickSelect(selected: ReadonlySet<string>, key: string): Selection {
  const next = new Set(selected);
  if (next.has(key)) next.delete(key);
  else next.add(key);
  return { selected: next, anchor: key };
}

/** Cmd or Ctrl+A: every row of the pane (unlike the header box, it does not clear when all are selected already). */
export const selectEverything = (keys: readonly string[]): Set<string> => new Set(keys);

/** The ticks of rows that are no longer in the list are dropped (after a read, a delete or a folder change). */
export const keepVisible = (keys: readonly string[], selected: ReadonlySet<string>): Set<string> => {
  const have = new Set(keys);
  return new Set([...selected].filter((k) => have.has(k)));
};

// ---- the marquee ----------------------------------------------------------------------------------------------------------------------

/** A row's place in the pane, measured from the top of the pane's content (so scrolling does not change it). */
export interface RowBox {
  key: string;
  top: number;
  bottom: number;
}

/** Below this many pixels of movement a press on empty space is a click (it leaves the selection unchanged), not a marquee. */
export const MARQUEE_SLOP = 4;

/** The rows that the band between `y1` and `y2` (either order) touches. */
export function marqueeKeys(rows: readonly RowBox[], y1: number, y2: number): string[] {
  const lo = Math.min(y1, y2);
  const hi = Math.max(y1, y2);
  return rows.filter((r) => r.bottom >= lo && r.top <= hi).map((r) => r.key);
}

/** The selection while a marquee is drawn: the selection from before the press plus the rows the band touches (a checklist: a band only adds). */
export const marqueeSelection = (base: ReadonlySet<string>, hit: readonly string[]): Set<string> =>
  new Set([...base, ...hit]);

/** How far the pane scrolls on each step while the pointer is near its top or bottom edge during a marquee (pixels; 0 away from the edges).
 * `top` and `bottom` are the visible area of the list in client coordinates, `y` the pointer. */
export function edgeScroll(y: number, top: number, bottom: number, zone = 36, max = 24): number {
  if (y < top + zone) return -Math.round((max * Math.min(zone, top + zone - y)) / zone);
  if (y > bottom - zone) return Math.round((max * Math.min(zone, y - (bottom - zone))) / zone);
  return 0;
}

// ---- the keys -------------------------------------------------------------------------------------------------------------------------

export type KeyCommand =
  | { type: 'all' }
  | { type: 'clear' }
  | { type: 'up' }
  | { type: 'open' }
  | { type: 'move'; delta: 1 | -1; extend: boolean };

/** What a key does in a focused pane: Cmd or Ctrl+A selects all, Esc clears, Cmd or Ctrl+Up and Backspace go up a folder, Enter and Cmd or
 * Ctrl+Down open the selected folder, the arrows move the selection by one row (Shift extends it). */
export function keyCommand(e: {
  key: string;
  metaKey: boolean;
  ctrlKey: boolean;
  shiftKey: boolean;
}): KeyCommand | undefined {
  const mod = e.metaKey || e.ctrlKey;
  switch (e.key) {
    case 'a':
    case 'A':
      return mod ? { type: 'all' } : undefined;
    case 'Escape':
      return { type: 'clear' };
    case 'Backspace':
      return { type: 'up' };
    case 'Enter':
      return { type: 'open' };
    case 'ArrowUp':
      return mod ? { type: 'up' } : { type: 'move', delta: -1, extend: e.shiftKey };
    case 'ArrowDown':
      return mod ? { type: 'open' } : { type: 'move', delta: 1, extend: e.shiftKey };
    default:
      return undefined;
  }
}

/** The selection after an arrow: one row down or up from the anchor's neighbour (the last row moved to); Shift extends the range from the anchor.
 * `cursor` is the row the last move or click ended on; nothing selected: the first (down) or last (up) row. */
export function moveSelect(
  keys: readonly string[],
  anchor: string | undefined,
  cursor: string | undefined,
  delta: 1 | -1,
  extend: boolean,
): (Selection & { cursor: string }) | undefined {
  if (!keys.length) return undefined;
  const at = cursor === undefined ? -1 : keys.indexOf(cursor);
  const to = at < 0 ? (delta > 0 ? 0 : keys.length - 1) : Math.min(keys.length - 1, Math.max(0, at + delta));
  const target = keys[to];
  if (extend) {
    const from = anchor !== undefined && keys.includes(anchor) ? anchor : target;
    return { selected: new Set(rangeKeys(keys, from, target)), anchor: from, cursor: target };
  }
  return { selected: new Set([target]), anchor: target, cursor: target };
}

// ---- dragging rows -------------------------------------------------------------------------------------------------------------------

/** The rows a drag that starts on `key` carries: the whole selection when the row is in it, else just that row. A drag never changes the
 * selection. Rows in list order. */
export function dragKeys(keys: readonly string[], selected: ReadonlySet<string>, key: string): string[] {
  return selected.has(key) ? keys.filter((k) => selected.has(k)) : [key];
}

/** Where a drop lands: on a device folder row, in that folder; anywhere else in the pane, in the open folder. */
export const dropFolder = (overFolder: string | undefined, open: string): string => overFolder ?? open;

/** What is being dragged over a pane, from the types of the drag: rows of the local list (our own drag), files from outside, or nothing we take. */
export const ROWS_TYPE = 'application/x-digikit-rows';
export function dragKind(types: readonly string[] | undefined): 'rows' | 'files' | 'none' {
  const t = Array.from(types ?? []);
  return t.includes(ROWS_TYPE) ? 'rows' : t.includes('Files') ? 'files' : 'none';
}
