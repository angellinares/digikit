// The parts of the Transfer page that Modules mode and Samples mode share: the two columns with their headers, the list row with its
// progress bar or verified/failed chip, the breadcrumb, and the two-click Delete button.
import { For, Show, createEffect, createSignal, type JSX } from 'solid-js';
import TargetSelect from './TargetSelect';
import { fmtRate } from './rate-core';
import type { PaneSelect } from './pane-select';
import { dragKind, selectState, toggleAll } from './select-core';
import { linkTag } from './link-core';
import { deleteLabel, freeLine, writing, type RowTag } from './transfer-core';
import { activeMode, deviceInfo, liveRate, sampleLink } from './transfer-state';

/** What the page gives each mode: the one busy action, and the error line. */
export interface PageCtx {
  busy: () => string;
  /** Run `f` as the page's one busy action; an error shows, and both sides are read again either way. */
  run: (what: string, f: () => Promise<void | CancelledRun>) => Promise<void>;
  setError: (m: string) => void;
  /** Changes when the page reads by itself (the target selector reads the MIDI ports again). */
  reload: () => number;
}

/** What a job that the user stopped returns (it is not a failure): what it did, as the log says it (`87 of 357 uploaded, 270 not sent`). */
export interface CancelledRun {
  cancelled: string;
}

/** What the page shows beside a file while it is sent: a bar, or a chip; `message` is the tooltip of the chip. */
export interface ChipStatus {
  kind: 'waiting' | 'uploading' | 'verifying' | 'ok' | 'same' | 'failed' | 'cancelled';
  sent?: number;
  bytes?: number;
  message?: string;
}

export function Chip(props: { status: ChipStatus }): JSX.Element {
  const pct = () => (props.status.bytes ? Math.round((100 * (props.status.sent ?? 0)) / props.status.bytes) : 0);
  const rate = () => {
    const r = liveRate();
    return r ? fmtRate(r) : '';
  };
  return (
    <>
      <Show when={props.status.kind === 'waiting'}>
        <span class="check idle">waiting</span>
      </Show>
      <Show when={props.status.kind === 'verifying'}>
        <span class="check idle">verifying…</span>
      </Show>
      <Show when={props.status.kind === 'ok'}>
        <span class="check ok" title={props.status.message}>
          verified
        </span>
      </Show>
      <Show when={props.status.kind === 'same'}>
        <span class="check ok" title={props.status.message}>
          on device
        </span>
      </Show>
      <Show when={props.status.kind === 'failed'}>
        <span class="check bad" title={props.status.message}>
          failed
        </span>
      </Show>
      <Show when={props.status.kind === 'cancelled'}>
        <span class="check idle" title={props.status.message}>
          cancelled
        </span>
      </Show>
      <Show when={props.status.kind === 'uploading'}>
        <span class="up" title={rate() ? `${pct()}% · ${rate()}` : `${pct()}%`}>
          <progress max={props.status.bytes} value={props.status.sent} />
          <span class="pct">{pct()}%</span>
          <span class="rate">{rate()}</span>
        </span>
      </Show>
    </>
  );
}

/** The small label after a name (`test`, `on device`, `WAV→`). */
function Tag(props: { tag: RowTag }): JSX.Element {
  return (
    <span class="rtag" classList={{ [props.tag.tone]: true }} title={props.tag.title}>
      {props.tag.text}
    </span>
  );
}

export interface ListRowProps {
  check?: { label: string; checked: boolean; disabled: boolean; onChange: () => void };
  /** The key of the row in the pane's selection (the marquee finds the row by it). */
  rowKey?: string;
  /** The row is selected (the tick is the selection). */
  selected?: boolean;
  /** A click on the row (not on the tick): the pane's click rules. */
  onSelect?: (e: MouseEvent) => void;
  name: string;
  /** The tooltip of the name. */
  title?: string;
  /** A folder: its name ends with a slash, and a double click opens it. */
  folder?: { disabled: boolean; onOpen: () => void };
  /** The row can be dragged out (the local rows). */
  drag?: (e: DragEvent) => void;
  /** A drop on this row goes into this folder of the device (a device folder row). */
  dropPath?: string;
  /** Small labels after the name; only what says something (a usual state has none). */
  tags?: RowTag[];
  /** The version (modules), a dim column before the size. */
  version?: string;
  /** The size, right-aligned. */
  size?: string;
  status?: ChipStatus;
  /** A small link-like button after the name ("Source"). */
  action?: { label: string; title: string; onClick: () => void };
}

/** The folder of the device that a drag is over (a folder row, or the Up row), or undefined; the row shows it as the drop target. */
const [dropHover, setDropHover] = createSignal<string>();

/** One row of a list in either column, always one line: tick, name with its tags, version, the bar or chip of a transfer, size. A click selects
 * it (Finder's rules), a double click opens a folder, and the text is not selectable. */
export function ListRow(props: ListRowProps): JSX.Element {
  return (
    <li
      data-key={props.rowKey}
      data-drop={props.dropPath}
      classList={{ sel: !!props.selected, dropon: props.dropPath !== undefined && dropHover() === props.dropPath }}
      draggable={props.drag ? true : undefined}
      onDragStart={(e) => props.drag?.(e)}
      onClick={(e) => {
        if (props.onSelect && !(e.target as Element).closest('input, button')) props.onSelect(e);
      }}
      onDblClick={(e) => {
        if (props.folder && !props.folder.disabled && !(e.target as Element).closest('input, button'))
          props.folder.onOpen();
      }}
    >
      <Show when={props.check} fallback={<span class="tick" />}>
        {(c) => (
          <input
            type="checkbox"
            aria-label={c().label}
            checked={c().checked}
            disabled={c().disabled}
            onChange={c().onChange}
          />
        )}
      </Show>
      <span class="name">
        <span class="nm" classList={{ dir: !!props.folder }} title={props.title}>
          {props.folder ? `${props.name}/` : props.name}
        </span>
        <For each={props.tags ?? []}>{(t) => <Tag tag={t} />}</For>
      </span>
      <Show when={props.action}>
        {(a) => (
          <button type="button" class="rowlink" title={a().title} onClick={a().onClick}>
            {a().label}
          </button>
        )}
      </Show>
      <Show when={props.version}>
        <span class="vers">{props.version}</span>
      </Show>
      <Show when={props.status}>{(st) => <Chip status={st()} />}</Show>
      <span class="size">{props.size ?? ''}</span>
    </li>
  );
}

/** The first row of a list inside a folder: "↑ Up" goes to the parent. A file dropped on it goes into the parent (`dropPath`). */
export function UpRow(props: { disabled: boolean; onUp: () => void; dropPath?: string; title: string }): JSX.Element {
  return (
    <ul class="rows uprows">
      <li
        class="uprow"
        data-drop={props.dropPath}
        classList={{ dropon: props.dropPath !== undefined && dropHover() === props.dropPath }}
        onClick={() => !props.disabled && props.onUp()}
      >
        <span class="tick" />
        <span class="name">
          <button type="button" class="nm upbtn" disabled={props.disabled} title={props.title}>
            ↑ Up
          </button>
        </span>
      </li>
    </ul>
  );
}

/** The select-all checkbox of a pane header: none, some (the dash) or all of `keys`, the visible rows that can be ticked. */
export function SelectAll(props: {
  label: string;
  keys: string[];
  ticked: Set<string>;
  disabled: boolean;
  onChange: (next: Set<string>) => void;
}): JSX.Element {
  let box: HTMLInputElement | undefined;
  const state = () => selectState(props.keys, props.ticked);
  createEffect(() => {
    if (box) box.indeterminate = state() === 'some';
  });
  return (
    <input
      ref={box}
      type="checkbox"
      class="all"
      aria-label={props.label}
      title={state() === 'all' ? 'Clear the selection' : 'Select all'}
      checked={state() === 'all'}
      disabled={props.disabled || props.keys.length === 0}
      onChange={() => props.onChange(toggleAll(props.keys, props.ticked))}
    />
  );
}

/** A folder path as buttons: one per level; the open level is the current location. */
export function Crumbs(props: {
  items: { name: string; path: string }[];
  current: string;
  disabled: boolean;
  label: string;
  onOpen: (path: string) => void;
}): JSX.Element {
  return (
    <nav class="crumbs" aria-label={props.label}>
      <For each={props.items}>
        {(c, i) => (
          <>
            <Show when={i() > 0}>
              <span class="dim">/</span>
            </Show>
            <button
              type="button"
              class="mini"
              aria-current={c.path === props.current ? 'location' : undefined}
              disabled={props.disabled || c.path === props.current}
              onClick={() => props.onOpen(c.path)}
            >
              {c.name}
            </button>
          </>
        )}
      </For>
    </nav>
  );
}

/** The two-click confirm of Delete: the first click arms it for about 3 s, the second (within that time) runs it. No modal. */
export function createConfirm() {
  const [confirming, setConfirming] = createSignal(false);
  let timer: number | undefined;
  const stop = () => {
    window.clearTimeout(timer);
    setConfirming(false);
  };
  const click = (go: () => void) => {
    if (!confirming()) {
      setConfirming(true);
      timer = window.setTimeout(() => setConfirming(false), 3000);
      return;
    }
    stop();
    go();
  };
  return { confirming, stop, click };
}

export function DeleteButton(props: {
  n: number;
  busy: string;
  confirming: boolean;
  /** The text once armed, when "Confirm delete (n)" is not enough (a folder goes with its contents). */
  confirmLabel?: string;
  title: string;
  onClick: () => void;
}): JSX.Element {
  return (
    <button
      type="button"
      class="act"
      classList={{ danger: props.confirming }}
      disabled={!!props.busy || props.n === 0}
      title={props.n ? props.title : 'Tick the rows to delete'}
      onClick={props.onClick}
    >
      {props.busy === 'Deleting'
        ? 'Deleting…'
        : props.confirming && props.confirmLabel
          ? props.confirmLabel
          : deleteLabel(props.n, props.confirming)}
    </button>
  );
}

/** The files of a drop from outside and how many folders it held (a folder arrives as an empty "file"; its contents cannot be read here). */
export function droppedFiles(e: DragEvent): { files: File[]; folders: number } {
  const dt = e.dataTransfer;
  const items = Array.from(dt?.items ?? []);
  const folders = items.filter((i) => i.kind === 'file' && i.webkitGetAsEntry?.()?.isDirectory).length;
  const files = items.length
    ? items.flatMap((i) => {
        const f = i.kind === 'file' && !i.webkitGetAsEntry?.()?.isDirectory ? i.getAsFile() : null;
        return f ? [f] : [];
      })
    : Array.from(dt?.files ?? []);
  return { files, folders };
}

/** The marquee the pane draws while a drag over empty space selects rows. */
function Marquee(props: { select?: PaneSelect }): JSX.Element {
  return (
    <Show when={props.select?.box()}>
      {(b) => (
        <div
          class="marquee"
          style={{ left: `${b().x}px`, top: `${b().y}px`, width: `${b().w}px`, height: `${b().h}px` }}
        />
      )}
    </Show>
  );
}

/** The handlers that make a pane an explorer list: focus for the keys, the marquee, and Cmd+A. */
const selectProps = (sel?: PaneSelect) =>
  sel ? { ref: sel.ref, tabindex: 0, onKeyDown: sel.onKeyDown, onPointerDown: sel.onPointerDown } : {};

/** The left column: what is on the computer. `actions` are its controls (they sit in its header); files dropped on it from outside add files.
 * `select` makes it an explorer list (click, Shift, Cmd, marquee, keys). */
export function LocalPane(props: {
  /** The select-all checkbox, before the heading. */
  selectAll: JSX.Element;
  actions: JSX.Element;
  onDrop: (files: File[], folders: number) => void;
  select?: PaneSelect;
  children: JSX.Element;
}): JSX.Element {
  const [over, setOver] = createSignal(false);
  return (
    <section
      class="col"
      classList={{ over: over() }}
      aria-label="On your computer"
      {...selectProps(props.select)}
      onDragOver={(e) => {
        // only files from outside are taken here; the rows of this list are dragged to the other pane
        if (dragKind(e.dataTransfer?.types) !== 'files') return;
        e.preventDefault();
        setOver(true);
      }}
      onDragLeave={(e) => {
        if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setOver(false);
      }}
      onDrop={(e) => {
        if (dragKind(e.dataTransfer?.types) !== 'files') return;
        e.preventDefault();
        setOver(false);
        const d = droppedFiles(e);
        props.onDrop(d.files, d.folders);
      }}
    >
      <div class="colhead">
        <div class="hl">
          {props.selectAll}
          <h3>On your computer</h3>
        </div>
        <div class="acts">{props.actions}</div>
      </div>
      <div class="listarea">{props.children}</div>
      <Marquee select={props.select} />
    </section>
  );
}

export { writing };

/** What a drop on the right column carries: rows of the local list (our own drag) or files from outside, and the device folder it was dropped
 * on (undefined: the open folder). */
export interface DeviceDrop {
  onDrop: (kind: 'rows' | 'files', folder: string | undefined, e: DragEvent) => void;
  /** A drop is not taken while a job runs. */
  disabled: () => boolean;
}

/** The right column: the target, the device and its list. Everything about the target is here: the selector, the info line and the buttons.
 * `drop` takes uploads dragged onto it (the pane, or one of its folder rows or the Up row, which light up). */
export function DevicePane(props: {
  ctx: PageCtx;
  selectAll: JSX.Element;
  actions: JSX.Element;
  select?: PaneSelect;
  drop?: DeviceDrop;
  /** Leave out the target selector and the device info line (a second device pane under the first one). */
  hideTarget?: boolean;
  children: JSX.Element;
}): JSX.Element {
  const [over, setOver] = createSignal(false);
  const clear = () => {
    setOver(false);
    setDropHover(undefined);
  };
  const taken = (e: DragEvent) =>
    props.drop && !props.drop.disabled() ? dragKind(e.dataTransfer?.types) : ('none' as const);
  const folderOf = (e: DragEvent): string | undefined =>
    (e.target as Element | null)?.closest?.('[data-drop]')?.getAttribute('data-drop') ?? undefined;
  return (
    <section
      class="col"
      classList={{ over: over() && dropHover() === undefined }}
      aria-label="On device"
      {...selectProps(props.select)}
      onDragOver={(e) => {
        if (taken(e) === 'none') return;
        e.preventDefault();
        if (e.dataTransfer) e.dataTransfer.dropEffect = 'copy';
        setOver(true);
        setDropHover(folderOf(e));
      }}
      onDragLeave={(e) => {
        if (!e.currentTarget.contains(e.relatedTarget as Node | null)) clear();
      }}
      onDrop={(e) => {
        const kind = taken(e);
        if (kind === 'none') return;
        e.preventDefault();
        const folder = folderOf(e);
        clear();
        props.drop?.onDrop(kind, folder, e);
      }}
    >
      <div class="colhead">
        <div class="hl">
          {props.selectAll}
          <h3>On device</h3>
        </div>
        <div class="acts">{props.actions}</div>
      </div>
      <Show when={!props.hideTarget}>
        <div class="devhead">
          <TargetSelect disabled={writing(props.ctx.busy())} onError={props.ctx.setError} reload={props.ctx.reload} />
          <Show when={deviceInfo()}>
            {(d) => (
              <>
                <Show when={d().free !== undefined}>
                  <span class="meta">{freeLine(d().free)}</span>
                </Show>
                <Show when={activeMode() === 'samples' && linkTag(sampleLink())}>{(t) => <Tag tag={t()} />}</Show>
                <Show when={d().storage}>
                  <span class="meta storage">{d().storage}</span>
                </Show>
              </>
            )}
          </Show>
        </div>
      </Show>
      <div class="listarea">{props.children}</div>
      <Marquee select={props.select} />
    </section>
  );
}
