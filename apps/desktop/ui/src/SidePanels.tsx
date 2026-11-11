// The right-hand column of the Emulator page and the Remote page: tabs over one panel at a time. The Emulator page offers Card, Memory and
// Metrics; the Remote page Memory alone. The column folds to a narrow rail so the faceplate takes the width back. The chosen tab and the fold are
// kept (panel-open.ts). Card and Memory read through the one session of their target (target.ts), and only while their tab is shown.
import { For, Show, type JSX } from 'solid-js';
import type { DkSession } from '@digi/dk-session';
import CardPanel from './CardPanel';
import MemoryPanel from './MemoryPanel';
import { TAB_LABEL, shownTab, type InspectorTab } from './inspector-core';
import { inspectorOpen, inspectorTab, setInspectorOpen, setInspectorTab } from './panel-open';
import './card-panel.css';

/**
 * `visible`: the page is shown (and, for the Emulator page, the emulator runs). `card`: offer the Card tab, with its `reload`.
 * `metrics`: offer the Metrics tab with this content. `session`, `where`, `track`: the Memory panel's (memory-panel props).
 */
export default function SidePanels(props: {
  visible: boolean;
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
  const shown = (t: InspectorTab) => props.visible && inspectorOpen() && tab() === t;
  const pick = (t: InspectorTab) => {
    setInspectorTab(t);
    setInspectorOpen(true);
  };
  return (
    <div class="side-col" classList={{ rail: !inspectorOpen() }} hidden={!props.visible}>
      <div class="side-tabs" role="tablist" aria-label="Inspector">
        <Show when={inspectorOpen()}>
          <For each={offered()}>
            {(t) => (
              <button
                type="button"
                role="tab"
                class="side-tab"
                classList={{ on: tab() === t }}
                aria-selected={tab() === t}
                onClick={() => pick(t)}
              >
                {TAB_LABEL[t]}
              </button>
            )}
          </For>
        </Show>
        <span class="sp" />
        <button
          type="button"
          class="side-fold"
          aria-expanded={inspectorOpen()}
          aria-label={inspectorOpen() ? 'Fold the column' : 'Open the column'}
          title={inspectorOpen() ? 'Fold the column' : `Open the column (${TAB_LABEL[tab()]})`}
          onClick={() => setInspectorOpen(!inspectorOpen())}
        >
          <svg viewBox="0 0 16 16" aria-hidden="true">
            <path
              d={inspectorOpen() ? 'M6 3L11 8L6 13' : 'M10 3L5 8L10 13'}
              fill="none"
              stroke="currentColor"
              stroke-width="1.6"
              stroke-linecap="round"
              stroke-linejoin="round"
            />
          </svg>
        </button>
      </div>
      <Show when={props.card}>
        <CardPanel visible={shown('card')} reload={props.reload} />
      </Show>
      <MemoryPanel visible={shown('memory')} session={props.session} where={props.where} track={props.track} />
      <Show when={props.metrics}>
        <div class="card-panel metrics-panel" role="tabpanel" aria-label="Metrics" hidden={!shown('metrics')}>
          {props.metrics}
        </div>
      </Show>
    </div>
  );
}
