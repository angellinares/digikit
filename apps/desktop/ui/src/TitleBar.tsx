// The title bar of the workbench, over every page: the name, then the one target selector the pages share (target.ts). Transfer, Remote, Firmware
// and the Memory panel all talk to the target picked here, so it sits above them rather than in each page (env.ts `targets.inHeader`).
import { onCleanup, onMount, Show, type JSX } from 'solid-js';
import ConnectionBadge from '@web/transfer/ConnectionBadge';
import TargetSelect, { rescanTargets } from '@web/transfer/TargetSelect';
import { canAutoRefresh, focusReadDue } from '@web/transfer/refresh-core';
import { target } from '@web/transfer/target';
import { writing } from '@web/transfer/transfer-core';
import { busy } from '@web/transfer/transfer-state';
import { running } from '@web/firmware/firmware-core';
import { run } from '@web/firmware/firmware-state';

// the title bar reads the ports by itself on every page, slower than the Transfer page's poll
const RESCAN_MS = 10_000;

export default function TitleBar(props: { onError?: (message: string) => void }): JSX.Element {
  const locked = () => writing(busy()) || running(run());
  let lastFocus: number | undefined;
  const free = () => canAutoRefresh(document.visibilityState === 'visible', busy()) && !running(run());
  const back = () => {
    const now = Date.now();
    if (!free() || !focusReadDue(lastFocus, now)) return;
    lastFocus = now;
    rescanTargets();
  };
  onMount(() => {
    window.addEventListener('focus', back);
    document.addEventListener('visibilitychange', back);
    const timer = window.setInterval(() => free() && rescanTargets(), RESCAN_MS);
    onCleanup(() => {
      window.removeEventListener('focus', back);
      document.removeEventListener('visibilitychange', back);
      window.clearInterval(timer);
    });
  });
  return (
    <header class="titlebar">
      <span class="brand">
        <span class="brand-mark" aria-hidden="true">
          <i />
          <i />
          <i />
          <i />
        </span>
        digikit
        <span class="brand-sub">workbench</span>
      </span>
      <span class="sp" />
      <Show when={target().kind !== 'none'}>
        <ConnectionBadge />
      </Show>
      {/* a running install, upload or firmware flash keeps its target: the selector is locked until it ends */}
      <TargetSelect disabled={locked()} onError={props.onError} />
    </header>
  );
}
