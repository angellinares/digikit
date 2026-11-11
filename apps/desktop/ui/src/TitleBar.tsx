// The title bar of the workbench, over every page: the name, then the one target selector the pages share (target.ts). Transfer, Remote, Firmware
// and the Memory panel all talk to the target picked here, so it sits above them rather than in each page (env.ts `targets.inHeader`).
import type { JSX } from 'solid-js';
import TargetSelect from '@web/transfer/TargetSelect';
import { writing } from '@web/transfer/transfer-core';
import { busy } from '@web/transfer/transfer-state';

export default function TitleBar(props: { onError?: (message: string) => void }): JSX.Element {
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
      {/* a running install or upload keeps its target: the selector is locked until it ends */}
      <TargetSelect disabled={writing(busy())} onError={props.onError} />
    </header>
  );
}
