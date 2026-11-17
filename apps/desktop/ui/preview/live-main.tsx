// The live preview's entry: a small desktop shell (the sidebar of src/Console.tsx, its page switch and its panels) over a running emulator
// (live.vite.config.ts). What the real Console does that the pages feel is kept: the faceplate stays mounted behind the other pages, the
// Memory panels are shown only on the Emulator page, and `status` is set once per display frame, as the runtime's pump sets it (a
// new object each time), so a panel that reads on every change of `status` is as busy here as in the app. The daemon is asked for the
// status twice a second only: a real call per frame would load it more than the app's pump does.
//   live.html?page=emulator|transfer|remote[&idle=1][&log=1]   window.__live: { page(name), frames() }
// `log=1` adds the Log drawer, following the running emulator's own runtime log (the device side has no port here).
// `idle=1` leaves the panels out (the status ticks stay): the baseline a measurement of the panels is compared with.
import { For, Show, createMemo, createSignal, onCleanup } from 'solid-js';
import { render } from 'solid-js/web';
import '@fontsource-variable/archivo/wdth.css';
import '@fontsource/jetbrains-mono/400.css';
import '@fontsource/jetbrains-mono/500.css';
import '@digi/workbench-core/ui/styles/site.css';
import '../src/console.css';
import '../src/native.css';
import { createDesktopApps } from '../src/desktop-app';
import { createTauriHost } from '../src/host/tauri-host';
import { installNativeFeel } from '../src/native';
import LivePanel from '@digi/workbench-core/ui/components/LivePanel.tsx';
import { panels } from '@digi/workbench-core/data/faceplates.ts';
import RemotePage from '../src/RemotePage';
import SidePanels from '../src/SidePanels';
import AppProvider from '@digi/workbench-core/ui/transfer/AppProvider.tsx';
import { createLogFollow, followSession } from '../src/log-follow';
import LogPanel from '../src/LogPanel';
import TransferPage from '@digi/workbench-core/ui/transfer/TransferPage.tsx';

const host = createTauriHost();
const apps = createDesktopApps(host);
const { emulatorSession } = apps.app.target;
type Page = 'emulator' | 'transfer' | 'remote';
const query = new URLSearchParams(location.search);
const start = (query.get('page') as Page | null) ?? 'emulator';
const idle = query.has('idle');
const withLog = query.has('log');
const call = async <T,>(method: string): Promise<T> =>
  (await (await fetch('/__emu', { method: 'POST', body: JSON.stringify({ method, params: {} }) })).json()) as T;

function Shell() {
  const [page, setPage] = createSignal<Page>(start);
  const [status, setStatus] = createSignal<{ device?: string; mode?: string }>();
  let frames = 0;
  let alive = true;
  onCleanup(() => (alive = false));
  let last: { device?: string; mode?: string } = {};
  type Raw = { mode?: string; cf?: { device?: string } };
  const poll = async () => {
    try {
      const raw = await call<Raw>('status');
      last = { device: raw.cf?.device, mode: raw.mode };
    } catch {
      /* the daemon is gone */
    }
  };
  void poll();
  const timer = window.setInterval(() => void poll(), 500);
  onCleanup(() => window.clearInterval(timer));
  const frame = () => {
    if (!alive) return;
    if (last.device) {
      setStatus({ ...last });
      frames++;
    }
    requestAnimationFrame(frame);
  };
  frame();
  // as Console.tsx: a memo, so the panels hear of a change of this value only
  const shown = createMemo(() => page() === 'emulator' && !!status());
  const device = () => (status()?.device === 'dt2' ? 'dt2' : 'dn2');
  const panel = () => panels[device()];
  (window as unknown as Record<string, unknown>).__live = {
    page: (p: Page) => setPage(p),
    frames: () => frames,
  };
  const follow = withLog
    ? createLogFollow({
        follow: () => true,
        flashing: () => false,
        emulator: { running: () => !!status(), session: async () => followSession(await emulatorSession()) },
        device: {
          port: () => undefined,
          session: () => Promise.reject(new Error('no port')),
          present: async () => false,
        },
      })
    : undefined;
  return (
    <div class="console" style={{ '--accent': panel().accent, '--func-legend': panel().func_legend }}>
      <div class="shell">
        <nav class="sidebar" aria-label="Pages">
          <button type="button" class="side toggle" aria-label="Collapse the sidebar">
            <span>«</span>
          </button>
          <For each={['emulator', 'transfer', 'remote'] as const}>
            {(p) => (
              <button type="button" class="side" classList={{ on: page() === p }} onClick={() => setPage(p)}>
                <span>{p[0].toUpperCase() + p.slice(1)}</span>
              </button>
            )}
          </For>
        </nav>
        <div class="content">
          <div class="emu-row" hidden={page() !== 'emulator'}>
            <main class="stage" aria-label={`${panel().name} front panel`}>
              <div class="surface">
                <LivePanel panel={panel()} runtime={() => undefined} onError={() => {}} />
              </div>
            </main>
            <Show when={!idle}>
              <SidePanels host={host} visible={shown()} session={emulatorSession} where={() => 'the emulator'} />
            </Show>
          </div>
          <Show when={page() === 'transfer'}>
            <TransferPage />
          </Show>
          <Show when={page() === 'remote'}>
            <RemotePage fallbackDevice={device()} emulatorLoaded={true} />
          </Show>
        </div>
      </div>
      <Show when={follow}>
        <section class="log-drawer" aria-label="Log" style={{ height: '420px' }}>
          <LogPanel
            host={host}
            follow={follow!}
            emulatorRunning={() => !!status()}
            devicePort={() => undefined}
            toggleKey="⌘J"
          />
        </section>
      </Show>
    </div>
  );
}

installNativeFeel();
render(
  () => (
    <AppProvider app={apps.app} firmware={apps.firmware}>
      <Shell />
    </AppProvider>
  ),
  document.getElementById('app')!,
);
