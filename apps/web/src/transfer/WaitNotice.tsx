// "Waiting for <target>…" with a Cancel: shown while a job runs, a request waits for the device's reply, and nothing has arrived for a moment
// (WAIT_QUIET_MS: no reply, no progress). An upload that makes progress never shows it. Cancel ends the wait of the session at once
// (`DkSession.cancelPending`); the request then fails as "Cancelled.". Used by the pages that talk to the target.
import { Show, createEffect, createSignal, on, onCleanup, type JSX } from 'solid-js';
import { quietMs } from './activity';
import { cancelPending, target, targetLabel } from './target';
import { waitShown, waitingLine } from './wait-core';

/** `cancel` false: the page has its own Cancel while the job runs (the bar's), so this line only says what it waits for. */
export default function WaitNotice(props: { active: boolean; cancel?: boolean }): JSX.Element {
  const [quiet, setQuiet] = createSignal<number>();
  const timer = window.setInterval(() => setQuiet(quietMs()), 250);
  onCleanup(() => window.clearInterval(timer));
  // a job that starts or ends: the silence measured for the one before says nothing about this one
  createEffect(
    on(
      () => props.active,
      () => setQuiet(undefined),
    ),
  );
  return (
    <Show when={waitShown(props.active, quiet())}>
      <p class="wait" role="status">
        <span>{waitingLine(targetLabel(target()))}</span>
        <Show when={props.cancel !== false}>
          <button type="button" class="act small" onClick={cancelPending}>
            Cancel
          </button>
        </Show>
      </p>
    </Show>
  );
}
