// The progress bar along the bottom of the Transfer page, the same for both modes: a label, the percent, the speed (smoothed over the last
// ~5 s) and the time left, over a thick track. The speed also goes to `liveRate`, which the row of the file being sent shows.
import { Show, onCleanup, type JSX } from 'solid-js';
import { createBarRate } from './bar-rate';
import { statsLine } from './rate-core';
import { setLiveRate } from './transfer-state';
import type { BarInfo } from './transfer-core';

/** `onCancel`: the job can be stopped; the Cancel button shows while it runs. */
export default function TransferBar(props: { bar: BarInfo; running: boolean; onCancel?: () => void }): JSX.Element {
  const speed = createBarRate(
    () => props.bar.sent,
    () => props.running,
    setLiveRate,
  );
  onCleanup(() => setLiveRate(undefined));
  const pct = () => Math.round(100 * props.bar.fraction);
  return (
    <footer class="bar" role="status" aria-label="Transfer progress">
      <div class="barline">
        <span class="msg" title={props.bar.message}>
          {props.bar.message}
        </span>
        <span class="stats">
          {statsLine(pct(), props.running, speed(), Math.max(0, props.bar.total - props.bar.sent))}
        </span>
        <Show when={props.running && props.onCancel}>
          <button type="button" class="act small" onClick={() => props.onCancel?.()}>
            Cancel
          </button>
        </Show>
      </div>
      <progress
        class="global"
        classList={{ failed: props.bar.failed }}
        max="100"
        value={pct()}
        aria-label="Overall progress"
      />
    </footer>
  );
}
