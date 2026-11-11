// The right-hand column of the Emulator page and the Remote page: tabs over one panel at a time. The Emulator page offers Card, Memory and
// Metrics; the Remote page Memory alone (its single tab shows as a title). The column folds to a narrow rail so the faceplate takes the width
// back; the rail lists the tabs vertically, and a click on one opens the column on it. The chosen tab and the fold are kept (panel-open.ts).
// Card and Memory read through the one session of their target (target.ts), and only while their tab is shown.
import { For, Show, createEffect, createSignal, on, type JSX } from 'solid-js';
import type { DkSession } from '@digi/dk-session';
import CardPanel from './CardPanel';
import MemoryPanel from './MemoryPanel';
import { TAB_LABEL, shownTab, type InspectorTab } from './inspector-core';
import { inspectorOpen, inspectorTab, setInspectorOpen, setInspectorTab } from './panel-open';
import './card-panel.css';

/**
 * `visible`: the column is on screen (the page is shown). `active`: the emulator runs, so Card and Memory can read (default true).
 * `card`: offer the Card tab, with its `reload`. `metrics`: offer the Metrics tab with this content. `session`, `where`, `track`: the
 * Memory panel's (memory-panel props).
 */
export default function SidePanels(props: {
  visible: boolean;
  active?: boolean;
  card?: boolean;
  reload?: () => Promise<void>;
  metrics?: JSX.Element;
  session: () => Promise<DkSession>;
  where: () => string;
  track?: () => unknown;
}): JSX.Element {
  const offered = (): InspectorTab[] => [
    ...(props.card ? (['card'] as const) : []),
    'memory',
    ...(props.metrics ? (['metrics'] as const) : []),
  ];
  const tab = () => shownTab(inspectorTab(), offered());
  const reads = (t: InspectorTab) => props.visible && (props.active ?? true) && inspectorOpen() && tab() === t;
  const tabpanel = (t: InspectorTab) =>
    offered().length > 1 ? { id: `side-panel-${t}`, labelledBy: `side-tab-${t}` } : undefined;
  const pick = (t: InspectorTab, focus = false) => {
    // a column with one tab must not overwrite the choice made on the Emulator page
    if (offered().length > 1) setInspectorTab(t);
    setInspectorOpen(true);
    if (focus) requestAnimationFrame(() => document.getElementById(`side-tab-${t}`)?.focus());
  };
  const onTabKey = (e: KeyboardEvent) => {
    const list = offered();
    const i = list.indexOf(tab());
    let next: InspectorTab;
    if (e.key === 'ArrowRight') next = list[(i + 1) % list.length]!;
    else if (e.key === 'ArrowLeft') next = list[(i - 1 + list.length) % list.length]!;
    else if (e.key === 'Home') next = list[0]!;
    else if (e.key === 'End') next = list[list.length - 1]!;
    else return;
    e.preventDefault();
    pick(next, true);
  };
  // the line under the chosen tab slides to it
  let tabsEl: HTMLDivElement | undefined;
  const [ink, setInk] = createSignal({ x: 0, w: 0 });
  // measured again when the column shows (a hidden column measures 0)
  createEffect(
    on([tab, inspectorOpen, () => offered().length, () => props.visible], () =>
      requestAnimationFrame(() => {
        const chosen = tabsEl?.querySelector<HTMLElement>('.side-tab.on');
        if (chosen) setInk({ x: chosen.offsetLeft, w: chosen.offsetWidth });
      }),
    ),
  );
  return (
    <div class="side-col" classList={{ rail: !inspectorOpen() }} hidden={!props.visible}>
      <div class="side-head">
        <Show when={inspectorOpen()}>
          <Show
            when={offered().length > 1}
            fallback={
              <span class="side-title" id={`side-tab-${tab()}`}>
                {TAB_LABEL[tab()]}
              </span>
            }
          >
            <div class="side-tabs" role="tablist" aria-label="Inspector" ref={tabsEl} onKeyDown={onTabKey}>
              <For each={offered()}>
                {(t) => (
                  <button
                    type="button"
                    role="tab"
                    id={`side-tab-${t}`}
                    aria-controls={`side-panel-${t}`}
                    aria-selected={tab() === t}
                    tabindex={tab() === t ? 0 : -1}
                    class="side-tab"
                    classList={{ on: tab() === t }}
                    onClick={() => pick(t)}
                  >
                    {TAB_LABEL[t]}
                  </button>
                )}
              </For>
              <span
                class="side-ink"
                aria-hidden="true"
                style={{ transform: `translateX(${ink().x}px)`, width: `${ink().w}px` }}
              />
            </div>
          </Show>
        </Show>
        <span class="sp" />
        <button
          type="button"
          class="side-fold"
          aria-expanded={inspectorOpen()}
          aria-label={inspectorOpen() ? 'Fold the column' : 'Open the column'}
          title={inspectorOpen() ? 'Fold the column' : `Open the column (${TAB_LABEL[tab()]})`}
          onClick={() => {
            const opening = !inspectorOpen();
            setInspectorOpen(opening);
            if (opening) requestAnimationFrame(() => document.getElementById(`side-tab-${tab()}`)?.focus());
          }}
        >
          <svg viewBox="0 0 16 16" aria-hidden="true">
            <path
              d="M6 3L11 8L6 13"
              fill="none"
              stroke="currentColor"
              stroke-width="1.6"
              stroke-linecap="round"
              stroke-linejoin="round"
            />
          </svg>
        </button>
      </div>
      <Show when={!inspectorOpen()}>
        <div class="side-rail-tabs">
          <For each={offered()}>
            {(t) => (
              <button
                type="button"
                class="side-rail-tab"
                classList={{ on: tab() === t }}
                title={`Open ${TAB_LABEL[t]}`}
                onClick={() => pick(t, true)}
              >
                {TAB_LABEL[t]}
              </button>
            )}
          </For>
        </div>
      </Show>
      <Show when={props.visible && inspectorOpen() && !(props.active ?? true) && tab() !== 'metrics'}>
        <p class="side-empty">The emulator is not running.</p>
      </Show>
      <Show when={props.card}>
        <CardPanel visible={reads('card')} reload={props.reload} tabpanel={tabpanel('card')} />
      </Show>
      <MemoryPanel
        visible={reads('memory')}
        session={props.session}
        where={props.where}
        track={props.track}
        tabpanel={tabpanel('memory')}
      />
      <Show when={props.metrics}>
        <div
          class="card-panel metrics-panel"
          role="tabpanel"
          id="side-panel-metrics"
          aria-labelledby="side-tab-metrics"
          hidden={!(props.visible && inspectorOpen() && tab() === 'metrics')}
        >
          {props.metrics}
        </div>
      </Show>
    </div>
  );
}
