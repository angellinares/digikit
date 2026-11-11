// The Memory panel: what the resources a device limits are used for. A tab of the right-hand column (SidePanels.tsx) of the Emulator page, beside
// Card and Metrics, and of the Remote page, alone; in the Card panel's look (card-panel.css). One bar per resource the device limits
// (memory-core.ts); each bar is split into one segment per loaded module, coloured by the module's type, then free space (a count with few slots,
// such as Machines, is one cell per slot instead); hover a
// segment for the module and its amount.
// The panel reads CAPACITY, MODULES and the local library (memory-backend.ts) over the session it is given, which is the target's one session
// (target.ts): when its tab is shown, when a Transfer job ended or the emulator restarted (the same signal the Card panel reads), when
// the window gets the focus, and with its Refresh button. One read at a time (read-gate-core.ts); a read that fails is tried again a few
// times with a growing gap (a device that is still booting after a restart). A panel whose tab is not shown reads nothing. Nothing pending or
// ticked is shown.
import { For, Show, createEffect, createMemo, createSignal, onCleanup, onMount, untrack, type JSX } from 'solid-js';
import { panels } from '@web/data/faceplates';
import { isCancelled, isNoRuntime } from '@web/modules/backend';
import type { DkSession } from '@digi/dk-session';
import { cardRefresh } from './card-state';
import { deviceOfPort } from './remote-core';
import { freeTitle, noteText, segmentTitle, slotCells, usedText, widthPct } from './memory-core';
import { readMemory, type MemoryReading } from './memory-backend';
import { createReadGate } from './read-gate-core';
import { focusReadDue } from '@web/transfer/refresh-core';
import { asked, noteReply } from '@web/transfer/connection-state';
import { checkRuntime, emulatorRuntime, sameTarget, target } from '@web/transfer/target';
import { failureLine } from '@web/transfer/wait-core';
import { reloadWithRuntime } from './emulator-actions';
import './card-panel.css';
import './memory-panel.css';

const LEGEND = [
  { kind: 'machine', text: 'Machine' },
  { kind: 'effect', text: 'Effect' },
  { kind: 'tool', text: 'Tool' },
  { kind: 'other', text: 'Other' },
] as const;

// The rows of a reading (memory-core.ts), shown greyed and empty while the emulator has no runtime.
const SKELETON_ROWS = ['DSP memory', 'CPU memory', 'Machines', 'Parameters', 'Pages', 'Modules'];

/**
 * `session`: the device to read (the emulator, or the selected target). `track`: read again when this changes (the selected target).
 * `where`: its name for an error line ("No reply from Emulator").
 */
export default function MemoryPanel(props: {
  visible: boolean;
  session: () => Promise<DkSession>;
  where: () => string;
  track?: () => unknown;
  /** Set when the panel is a tab of the column (SidePanels.tsx): the tabpanel's id and the id of its tab. */
  tabpanel?: { id: string; labelledBy: string };
}): JSX.Element {
  const [reading, setReading] = createSignal<MemoryReading>();
  const [error, setError] = createSignal('');
  const [busy, setBusy] = createSignal(false);
  const [noRuntime, setNoRuntime] = createSignal(false);

  // the Emulator page passes the emulator's session; the Remote page passes the selected target's
  const readsEmulator = () => props.where() === 'the emulator';
  // The Emulator page's panel reads the emulator whatever the target is; only a read of the selected target may move the connection indicator.
  const readsTarget = () => !readsEmulator() || target().kind === 'emulator';
  const emulatorRead = () => readsEmulator() || target().kind === 'emulator';
  const gate = createReadGate({
    run: async (current) => {
      setBusy(true);
      const t0 = target();
      try {
        const check = () => checkRuntime(readsEmulator() ? { kind: 'emulator' } : target());
        if (readsTarget()) await asked(async () => check());
        else check();
        const s = await props.session();
        const r = readsTarget() ? await asked(() => readMemory(s)) : await readMemory(s);
        if (current()) {
          setReading(r);
          setError('');
          setNoRuntime(false);
          if (readsTarget()) void noteReply(s, () => sameTarget(t0, target()));
        }
        return true;
      } catch (e) {
        if (!current()) return true;
        // the Cancel button ended it: nothing is wrong, and nothing tries again by itself
        if (isCancelled(e)) return true;
        // nothing changes until the emulator boots again: no retry
        if (isNoRuntime(e)) {
          setError(failureLine(e, props.where()));
          setNoRuntime(true);
          return true;
        }
        setError(failureLine(e, props.where()));
        return false;
      } finally {
        setBusy(false);
      }
    },
  });
  // Only a change of whether the panel is on screen and open counts, not each change of what `visible` reads (the Console's `status` changes
  // once per display frame): a memo passes on a change of its value only.
  const active = createMemo(() => props.visible);
  // read when the panel is shown and opened, when the target changed, when a Transfer job ended or the emulator restarted
  createEffect(() => {
    cardRefresh();
    props.track?.();
    emulatorRuntime();
    if (active()) untrack(() => gate.request());
    else untrack(() => gate.stop());
  });
  let lastFocusRead: number | undefined;
  const back = () => {
    if (document.visibilityState !== 'visible' || !active()) return;
    const now = Date.now();
    if (!focusReadDue(lastFocusRead, now)) return;
    lastFocusRead = now;
    gate.request();
  };
  onMount(() => {
    window.addEventListener('focus', back);
    document.addEventListener('visibilitychange', back);
  });
  onCleanup(() => {
    gate.dispose(); // an answer after the panel is gone is dropped
    window.removeEventListener('focus', back);
    document.removeEventListener('visibilitychange', back);
  });

  const device = () => {
    const d = deviceOfPort(reading()?.port ?? '');
    return d ? panels[d].name : '';
  };

  return (
    <aside
      class="card-panel memory-panel"
      id={props.tabpanel?.id}
      role={props.tabpanel ? 'tabpanel' : undefined}
      aria-labelledby={props.tabpanel?.labelledBy}
      aria-label={props.tabpanel ? undefined : 'Memory'}
      hidden={!props.visible}
    >
      <div class="card-body">
        <Show when={device()}>
          <h3 class="device">{device()}</h3>
        </Show>
        <Show when={reading()}>
          {(r) => (
            <>
              <For each={r().bars}>
                {(bar) => (
                  <div class="mem-bar">
                    <div class="mem-head">
                      <span>{bar.label}</span>
                      <span class="used">{usedText(bar)}</span>
                    </div>
                    <Show
                      when={slotCells(bar)}
                      fallback={
                        <div
                          class="bar"
                          classList={{ over: bar.over }}
                          role="img"
                          aria-label={`${bar.label}: ${usedText(bar)}`}
                        >
                          <For each={bar.segments}>
                            {(s) => (
                              <span
                                class={`seg ${s.kind}`}
                                style={{ width: `${widthPct(bar, s.amount)}%` }}
                                title={segmentTitle(bar, s)}
                              />
                            )}
                          </For>
                          <span class="seg free" title={freeTitle(bar)} />
                        </div>
                      }
                    >
                      {(cells) => (
                        <div
                          class="cells"
                          classList={{ over: bar.over }}
                          role="img"
                          aria-label={`${bar.label}: ${usedText(bar)}`}
                        >
                          <For each={cells()}>
                            {(c) => <span class={`cell ${c.kind}`} classList={{ over: c.over }} title={c.title} />}
                          </For>
                        </div>
                      )}
                    </Show>
                    <Show when={noteText(bar)}>
                      {(note) => (
                        <span class="mem-note" classList={{ over: bar.over }}>
                          {note()}
                        </span>
                      )}
                    </Show>
                  </div>
                )}
              </For>
              <Show when={r().loaded === 0}>
                <p class="empty">No module is loaded.</p>
              </Show>
              <ul class="legend" aria-label="Module types">
                <For each={LEGEND}>
                  {(l) => (
                    <li>
                      <span class={`swatch ${l.kind}`} />
                      {l.text}
                    </li>
                  )}
                </For>
              </ul>
            </>
          )}
        </Show>
        <Show when={error()}>
          <p class="err" role="alert">
            {error()}
          </p>
        </Show>
        <Show when={noRuntime() && !reading()}>
          <div class="skeleton" aria-hidden="true">
            <For each={SKELETON_ROWS}>
              {(label) => (
                <div class="mem-bar">
                  <div class="mem-head">
                    <span>{label}</span>
                    <span class="used">—</span>
                  </div>
                  <div class="bar">
                    <span class="seg free" />
                  </div>
                </div>
              )}
            </For>
          </div>
        </Show>
        <Show when={noRuntime() && emulatorRead()}>
          <div class="acts">
            <button type="button" class="act small" onClick={() => reloadWithRuntime()}>
              Reload with runtime
            </button>
          </div>
        </Show>
        <div class="acts">
          <button type="button" class="act small" disabled={busy()} onClick={() => gate.request()}>
            {busy() ? 'Reading…' : 'Refresh'}
          </button>
        </div>
      </div>
    </aside>
  );
}
