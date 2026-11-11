// The Remote page (desktop): the front panel of the selected target with its real screen, the keys and the encoders working on the device.
// The screen is the firmware's own Screenshot (stock MidiRpc 0x04), polled; keys and encoders are the commands of digikit/remote (KEY, TURN),
// which work while digikit/remote is loaded at the device. The page speaks the protocol itself in TypeScript over the target of
// target.ts: the emulator's USB capture or a MIDI port. No process is started.
import { Show, createSignal, onCleanup, onMount, type JSX } from 'solid-js';
import LivePanel, { type LivePanelApi } from '@web/components/LivePanel';
import { panels } from '@web/data/faceplates';
import type { DkSession, RemoteState } from '@digi/dk-session';
import {
  deviceOfPort,
  infoDue,
  isRemoteMissing,
  leaveCancels,
  remoteNotice,
  remotePaused,
  remoteRuntime,
  remoteStateWanted,
} from './remote-core';
import { windowShown } from './log-follow-core';
import SidePanels from './SidePanels';
import { asked, noteReply } from '@web/transfer/connection-state';
import {
  cancelPending,
  checkRuntime,
  resetSession,
  sameTarget,
  session,
  target,
  targetLabel,
} from '@web/transfer/target';
import { busy } from '@web/transfer/transfer-state';
import { failureLine } from '@web/transfer/wait-core';
import './remote-page.css';

const SCREEN_MS = { emulator: 250, midi: 400 };
const STATE_MS = 2000;
/** How long the window may stay hidden before the loops look again. */
const HIDDEN_MS = 500;
// Keys and knobs are LivePanel's (the knobs turn by drag and wheel), so the page has no extra buttons for them.
const HINT = 'Click keys; drag or scroll knobs. Shift: ×10.';

export default function RemotePage(props: { fallbackDevice: 'dt2' | 'dn2' }): JSX.Element {
  const [state, setState] = createSignal<RemoteState>();
  const [stateError, setStateError] = createSignal<unknown>();
  const [screenError, setScreenError] = createSignal('');
  // oxlint-disable-next-line solid/reactivity -- the emulator's device is only the first guess; the target's INFO names the device
  const [device, setDevice] = createSignal<'dt2' | 'dn2'>(props.fallbackDevice);
  let live: LivePanelApi | undefined;
  let alive = true;
  const panel = () => panels[device()];
  const runtime = remoteRuntime(() => session());
  const notice = () => remoteNotice(state(), stateError());

  /** Screen loop: one Screenshot after the other; an error shows and the loop tries again slowly. */
  const screenLoop = async () => {
    while (alive) {
      // nothing is asked of the device while the window is hidden or a Transfer job uses the session
      if (!windowShown() || remotePaused(busy())) {
        await new Promise((r) => setTimeout(r, HIDDEN_MS));
        continue;
      }
      const t = target();
      try {
        const s: DkSession = await session();
        const shot = await s.screen();
        live?.showFrame(shot);
        setScreenError('');
        await new Promise((r) => setTimeout(r, SCREEN_MS[t.kind === 'midi' ? 'midi' : 'emulator']));
      } catch (e) {
        setScreenError(failureLine(e, targetLabel(target())));
        resetSession();
        await new Promise((r) => setTimeout(r, 1500));
      }
    }
  };
  const stateLoop = async () => {
    // set when the device answered E_PROTO: from then on the MODULES list says when digikit/remote is there (no failing request every poll)
    let missing = false;
    // INFO only names the device (dt2 or dn2) for the faceplate: ask when the target changed, after an error and now and then, not every poll
    let infoKey = '';
    let infoAt: number | undefined;
    while (alive) {
      if (!windowShown() || remotePaused(busy())) {
        await new Promise((r) => setTimeout(r, HIDDEN_MS));
        continue;
      }
      const t = target();
      try {
        await asked(async () => checkRuntime(target()));
        const s = await session();
        const key = targetLabel(target());
        if (infoDue(infoAt, Date.now(), key !== infoKey)) {
          const info = await s.info(true);
          infoKey = key;
          infoAt = Date.now();
          const d = deviceOfPort(info.port);
          if (d && d !== device()) setDevice(d);
        }
        const loaded = missing ? await s.modules().catch(() => undefined) : undefined;
        if (remoteStateWanted(missing, loaded)) {
          setState(await s.remoteState());
          setStateError(undefined);
          missing = false;
          void noteReply(s, () => sameTarget(t, target()));
        }
      } catch (e) {
        missing = isRemoteMissing(e);
        infoAt = undefined;
        setState(undefined);
        setStateError(e);
      }
      await new Promise((r) => setTimeout(r, STATE_MS));
    }
  };
  onMount(() => {
    void screenLoop();
    void stateLoop();
  });
  onCleanup(() => {
    alive = false;
    // a poll that waits for a silent device must not hold up the next page's requests (they take turns on the session); a Transfer job's
    // requests are not ours to cancel
    if (leaveCancels(busy())) cancelPending();
    live?.releaseAll();
    // let go of the keys the page pressed (the module remembers them)
    if (state())
      void session()
        .then((s) => s.remoteRelease())
        .catch(() => {});
  });

  return (
    <section class="remote-page" aria-label="Remote">
      <header class="head">
        <h2>Remote</h2>
        <span class="meta">
          {targetLabel(target())} · {device()}
        </span>
        <span class="hints" title={HINT}>
          <span>
            <kbd>Click</kbd> keys
          </span>
          <span>
            <kbd>Drag</kbd> or <kbd>Scroll</kbd> knobs
          </span>
          <span>
            <kbd>⇧</kbd> ×10
          </span>
        </span>
      </header>
      <Show when={remotePaused(busy())}>
        <p class="note" role="status">
          Paused during the transfer.
        </p>
      </Show>
      <Show when={notice()}>
        <p class="note" role="status">
          {notice()}
        </p>
      </Show>
      <Show when={screenError()}>
        <p class="err" role="alert">
          {screenError()}
        </p>
      </Show>
      {/* the same markup as the Emulator page's faceplate (Console.tsx), so the screen has the same place and scale; the side column with the Memory panel is beside it */}
      <div class="remote-row">
        <main
          class="stage"
          aria-label={`${panel().name} front panel`}
          title={HINT}
          style={{ '--accent': panel().accent, '--func-legend': panel().func_legend }}
        >
          <div class="surface">
            <LivePanel panel={panel()} runtime={() => runtime} onError={setScreenError} ref={(api) => (live = api)} />
          </div>
        </main>
        <SidePanels visible session={session} where={() => targetLabel(target())} track={target} />
      </div>
    </section>
  );
}
