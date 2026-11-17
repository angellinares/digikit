// The selection behaviour of a list pane (Solid and the DOM; the rules are in select-core.ts, which node tests): a click on a row toggles it
// (Shift: add a range), a press on empty space draws a marquee that adds the rows it touches (the pane scrolls at its edges);
// a click on empty space changes nothing. Esc and the select-all checkbox clear, Cmd or Ctrl+A selects all, the arrows move, Enter and Backspace open and go up. The tick of a row is its selection.
// Used by both panes of both modes; `TransferParts.tsx` puts `handlers`, `ref` and the marquee box on the pane.
import { createSignal } from 'solid-js';
import {
  MARQUEE_SLOP,
  ROWS_TYPE,
  clickSelect,
  dragKeys,
  edgeScroll,
  keyCommand,
  marqueeKeys,
  marqueeSelection,
  modsOf,
  moveSelect,
  selectEverything,
  tickSelect,
  type RowBox,
} from './select-core';

export interface PaneSelectOptions {
  /** The rows that can be selected, in list order. */
  keys: () => string[];
  selected: () => Set<string>;
  set: (next: Set<string>) => void;
  /** Nothing is selected or opened while a job runs. */
  disabled: () => boolean;
  /** Enter on one selected row: open it when it is a folder. */
  open?: (key: string) => void;
  /** Cmd+Up, Backspace: go up one folder (when the pane has folders). */
  up?: () => void;
}

export interface Box {
  x: number;
  y: number;
  w: number;
  h: number;
}

/** What a pane needs: the handlers for its element, the marquee box to draw, and what a row calls. */
export function createPaneSelect(o: PaneSelectOptions) {
  let pane: HTMLElement | undefined;
  let anchor: string | undefined;
  let cursor: string | undefined;
  const [box, setBox] = createSignal<Box>();

  const rowEls = () => Array.from(pane?.querySelectorAll<HTMLElement>('li[data-key]') ?? []);
  const boxes = (): RowBox[] => {
    if (!pane) return [];
    const top = pane.getBoundingClientRect().top - pane.scrollTop;
    return rowEls().map((el) => {
      const r = el.getBoundingClientRect();
      return { key: el.dataset.key as string, top: r.top - top, bottom: r.bottom - top };
    });
  };
  const reveal = (key: string) =>
    rowEls()
      .find((el) => el.dataset.key === key)
      ?.scrollIntoView({ block: 'nearest' });

  /** A click on a row (not on its tick or a button): a checklist, a plain click toggles the row. */
  const click = (key: string, e: MouseEvent) => {
    if (o.disabled()) return;
    const r = clickSelect(o.keys(), o.selected(), anchor, key, modsOf(e));
    anchor = r.anchor;
    cursor = key;
    o.set(r.selected);
    pane?.focus({ preventScroll: true });
  };
  /** The tick of a row toggles that row, as a Cmd-click does. */
  const tick = (key: string) => {
    if (o.disabled()) return;
    const r = tickSelect(o.selected(), key);
    anchor = cursor = key;
    o.set(r.selected);
  };
  /** A drag starts on a row: the selection goes along when the row is in it, else the row alone. The selection does not change. */
  const dragStart = (key: string, e: DragEvent) => {
    const d = { keys: dragKeys(o.keys(), o.selected(), key) };
    const dt = e.dataTransfer;
    if (!dt) return;
    dt.setData(ROWS_TYPE, JSON.stringify(d.keys));
    dt.effectAllowed = 'copy';
    const chip = document.createElement('div');
    chip.className = 'dragchip';
    chip.textContent = d.keys.length === 1 ? (d.keys[0].split('/').pop() ?? '') : `${d.keys.length} items`;
    document.body.append(chip);
    dt.setDragImage(chip, -8, -8);
    setTimeout(() => chip.remove(), 0);
  };

  const onKeyDown = (e: KeyboardEvent) => {
    const t = e.target as Element;
    if (o.disabled() || t.closest('input[type=text], textarea, select')) return;
    const cmd = keyCommand(e);
    if (!cmd) return;
    switch (cmd.type) {
      case 'all':
        o.set(selectEverything(o.keys()));
        break;
      case 'clear':
        o.set(new Set());
        break;
      case 'up':
        if (!o.up) return;
        o.up();
        break;
      case 'open': {
        // Enter on a focused button presses that button
        const sel = [...o.selected()];
        if (t.closest('button') || sel.length !== 1 || !o.open) return;
        o.open(sel[0]);
        break;
      }
      case 'move': {
        const m = moveSelect(o.keys(), anchor, cursor, cmd.delta, cmd.extend);
        if (!m) return;
        anchor = m.anchor;
        cursor = m.cursor;
        o.set(m.selected);
        reveal(m.cursor);
        break;
      }
    }
    e.preventDefault();
  };

  const onPointerDown = (e: PointerEvent) => {
    const el = pane;
    if (!el || e.button !== 0 || o.disabled()) return;
    const t = e.target as Element;
    if (t.closest('li, button, input, select, label, a, form, progress, .colhead, .devhead, .crumbs')) return;
    // empty space: no text selection and no native drag from here; keys go to this pane
    e.preventDefault();
    el.focus({ preventScroll: true });
    el.setPointerCapture(e.pointerId);
    const base = o.selected();
    const origin = el.getBoundingClientRect();
    const start = { x: e.clientX - origin.left + el.scrollLeft, y: e.clientY - origin.top + el.scrollTop };
    const press = { x: e.clientX, y: e.clientY };
    let last = { x: e.clientX, y: e.clientY };
    let moved = false;
    let timer: number | undefined;
    const update = () => {
      const r = el.getBoundingClientRect();
      const cur = { x: last.x - r.left + el.scrollLeft, y: last.y - r.top + el.scrollTop };
      setBox({
        x: Math.min(start.x, cur.x),
        y: Math.min(start.y, cur.y),
        w: Math.abs(cur.x - start.x),
        h: Math.abs(cur.y - start.y),
      });
      o.set(marqueeSelection(base, marqueeKeys(boxes(), start.y, cur.y)));
    };
    const autoscroll = () => {
      const r = el.getBoundingClientRect();
      const head = el.querySelector<HTMLElement>('.colhead')?.offsetHeight ?? 0;
      const dy = edgeScroll(last.y, r.top + head, r.bottom);
      if (dy) {
        const before = el.scrollTop;
        el.scrollTop += dy;
        if (el.scrollTop !== before) update();
      }
    };
    const move = (m: PointerEvent) => {
      last = { x: m.clientX, y: m.clientY };
      if (!moved && Math.hypot(last.x - press.x, last.y - press.y) >= MARQUEE_SLOP) {
        moved = true;
        timer = window.setInterval(autoscroll, 16);
      }
      if (moved) update();
    };
    const end = () => {
      el.removeEventListener('pointermove', move);
      el.removeEventListener('pointerup', end);
      el.removeEventListener('pointercancel', end);
      window.clearInterval(timer);
      if (el.hasPointerCapture(e.pointerId)) el.releasePointerCapture(e.pointerId);
      setBox(undefined);
    };
    el.addEventListener('pointermove', move);
    el.addEventListener('pointerup', end);
    el.addEventListener('pointercancel', end);
  };

  return {
    box,
    click,
    tick,
    dragStart,
    /** Put on the pane element. */
    ref: (el: HTMLElement) => (pane = el),
    onKeyDown,
    onPointerDown,
  };
}

export type PaneSelect = ReturnType<typeof createPaneSelect>;
