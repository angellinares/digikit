// The Memory panel: what the resources a device limits are used for. A collapsible panel in the Card panel's look (card-panel.css), under the
// Card panel in the right-hand column of the Emulator page (SidePanels.tsx) and alone in the Remote page's. One bar per resource the device
// limits (memory-core.ts); each bar is split into one segment per loaded module, coloured by the module's type, then free space; hover a
// segment for the module and its amount.
// The panel reads CAPACITY, MODULES and the local library (memory-backend.ts) over the session it is given, which is the target's one session
// (target.ts): when it is shown or opened, when a Transfer job ended or the emulator restarted (the same signal the Card panel reads), when
// the window gets the focus, and with its Refresh button. One read at a time (read-gate-core.ts); a read that fails is tried again a few
// times with a growing gap (a device that is still booting after a restart). A hidden or collapsed panel reads nothing. Nothing pending or
// ticked is shown.
import { For, Show, createEffect, createMemo, createSignal, onCleanup, onMount, untrack, type JSX } from 'solid-js';
import { panels } from '@web/data/faceplates';
import { isCancelled } from '@web/modules/backend';
import type { DkSession } from '@digi/dk-session';
import { cardRefresh } from './card-state';
import { memoryOpen as open, setMemoryOpen } from './panel-open';
import { deviceOfPort } from './remote-core';
import { freeTitle, noteText, segmentTitle, usedText, widthPct } from './memory-core';
import { readMemory, type MemoryReading } from './memory-backend';
import { createReadGate } from './read-gate-core';
import { focusReadDue } from '@web/transfer/refresh-core';
import { failureLine } from '@web/transfer/wait-core';
import './card-panel.css';
import './memory-panel.css';

const LEGEND = [
  { kind: 'machine', text: 'Machine' },
  { kind: 'effect', text: 'Effect' },
  { kind: 'tool', text: 'Tool' },
  { kind: 'other', text: 'Other' },
] as const;

/**
 * `session`: the device to read (the emulator, or the selected target). `track`: read again when this changes (the selected target).
 * `where`: its name for an error line ("No reply from Emulator").
 */
export default function MemoryPanel(props: {
  visible: boolean;
  session: () => Promise<DkSession>;
  where: () => string;
  track?: () => unknown;
}): JSX.Element {
  const [reading, setReading] = createSignal<MemoryReading>();
  const [error, setError] = createSignal('');
  const [busy, setBusy] = createSignal(false);

  const gate = createReadGate({
    run: async (current) => {
      setBusy(true);
      try {
        const r = await readMemory(await props.session());
        if (current()) {
          setReading(r);
          setError('');
        }
        return true;
      } catch (e) {
        if (!current()) return true;
        // the Cancel button ended it: nothing is wrong, and nothing tries again by itself
        if (isCancelled(e)) return true;
        setError(failureLine(e, props.where()));
        return false;
      } finally {
        setBusy(false);
      }
    },
  });
  // Only a change of whether the panel is on screen and open counts, not each change of what `visible` reads (the Console's `status` changes
  // once per display frame): a memo passes on a change of its value only.
  const active = createMemo(() => props.visible && open());
  // read when the panel is shown and opened, when the target changed, when a Transfer job ended or the emulator restarted
  createEffect(() => {
    cardRefresh();
    props.track?.();
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

  const toggle = () => setMemoryOpen(!open());
  const device = () => {
    const d = deviceOfPort(reading()?.port ?? '');
    return d ? panels[d].name : '';
  };

  return (
    <aside class="card-panel memory-panel" classList={{ closed: !open() }} aria-label="Memory" hidden={!props.visible}>
      <button
        type="button"
        class="card-toggle"
        aria-expanded={open()}
        title={open() ? 'Collapse the Memory panel' : 'Show the Memory panel'}
        onClick={toggle}
      >
        <svg viewBox="0 0 16 16" aria-hidden="true">
          <path
            d={open() ? 'M6 3L11 8L6 13' : 'M10 3L5 8L10 13'}
            fill="none"
            stroke="currentColor"
            stroke-width="1.6"
            stroke-linecap="round"
            stroke-linejoin="round"
          />
        </svg>
        <strong>Memory</strong>
      </button>
      <Show when={open()}>
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
          <div class="acts">
            <button type="button" class="act small" disabled={busy()} onClick={() => gate.request()}>
              {busy() ? 'Reading…' : 'Refresh'}
            </button>
          </div>
        </div>
      </Show>
    </aside>
  );
}
