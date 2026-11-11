// The preview entry: the desktop shell (title bar, sidebar, content) around the real Transfer page, over the fake device of sim.ts.
// Mirrors the markup of ../src/Console.tsx for the parts around the page, so widths and heights are the app's.
import { render } from 'solid-js/web';
import '@fontsource-variable/archivo/wdth.css';
import '@fontsource/jetbrains-mono/400.css';
import '@fontsource/jetbrains-mono/500.css';
import '@web/styles/site.css';
import '../src/console.css';
import '../src/native.css';
import '../src/desktop-env';
import { desktopDefaultModules } from '../src/default-modules';
import { installNativeFeel } from '../src/native';
import DefaultModules from '@web/firmware/DefaultModules';
import { DefaultModuleError } from '@web/firmware/default-modules-core';
import { firmwareEnv, setFirmwareEnv } from '@web/firmware/env';
import LivePanel from '@web/components/LivePanel';
import { panels } from '@web/data/faceplates';
import FirmwarePage from '@web/firmware/FirmwarePage';
import RemotePage from '../src/RemotePage';
import SidePanels from '../src/SidePanels';
import TitleBar from '../src/TitleBar';
import { setLocalFolder } from '../src/samples-state';
import TransferPage from '@web/transfer/TransferPage';
import { setMode } from '@web/transfer/transfer-state';
import { deviceFromName, bootingStatus, booting as startBoot, loadFailure } from '../src/boot-core';
import { cardDevice, reloadCard } from './card-mock';
import { installRemoteMock, testPattern } from './remote-mock';
import { queryFrame } from './frame-from-png';
import { createLogFollow } from '../src/log-follow';
import LogPanel from '../src/LogPanel';
import { setLogOpen } from '../src/log-state';
import { logMode, previewLog } from './runtime-log-mock';
import { SAMPLES_ROOT, control, sim } from './sim';

// ?page=transfer (default) | remote | emulator | firmware (state: fwdev, fwstate, fwos; see firmware-mock.ts): the Emulator page shows the faceplate only (the real one needs the emulator) with the same
// markup as ../src/Console.tsx, to put its screen next to the Remote page's.
const page = new URLSearchParams(location.search).get('page') ?? 'transfer';
if (page === 'remote') installRemoteMock();
// ?boot=loading | failed: the Emulator page while a firmware starts (the old device information is gone), or after a failed load
// (the name of the file is `bootfile`, default Digitakt_II_OS1.17.syx). The texts are the app's (../src/boot-core.ts).
const bootState = new URLSearchParams(location.search).get('boot');
const bootFile = new URLSearchParams(location.search).get('bootfile') ?? 'Digitakt_II_OS1.17.syx';
const bootError = 'the emulator did not answer within 120 s';
const emulatorFaceplate = () =>
  bootState ? (
    <div class="emu-row">
      <main class="stage" aria-label="front panel">
        <div class="surface">
          <LivePanel
            panel={panels[deviceFromName(bootFile) ?? 'dn2']}
            runtime={() => undefined}
            onError={() => {}}
            ref={() => {}}
          />
        </div>
      </main>
    </div>
  ) : (
    <div class="emu-row">
      <main class="stage" aria-label="front panel">
        <div class="surface">
          <LivePanel
            panel={panels[cardDevice]}
            runtime={() => undefined}
            onError={() => {}}
            ref={(api) => void queryFrame().then((f) => api.showFrame(f ?? testPattern()))}
          />
        </div>
      </main>
      <SidePanels
        visible
        card
        reload={reloadCard}
        session={() => Promise.resolve({} as never)}
        where={() => 'the emulator'}
      />
    </div>
  );

/** The Log drawer under an empty page, as in the app: the same panel, fed by the fake runtime logs. */
function LogPage() {
  setLogOpen(true);
  const follow = createLogFollow({
    follow: () => true,
    flashing: () => false,
    emulator: { running: previewLog.emulatorRunning, session: async () => previewLog.emulator },
    device: { port: previewLog.devicePort, session: async () => previewLog.device, present: async () => true },
  });
  return (
    <section
      class="log-drawer"
      aria-label="Log"
      style={{ height: `${Number(new URLSearchParams(location.search).get('logh') ?? 360)}px` }}
    >
      <LogPanel
        follow={follow}
        emulatorRunning={previewLog.emulatorRunning}
        devicePort={previewLog.devicePort}
        toggleKey="⌘J"
      />
    </section>
  );
}

// The default modules install on a fake device: ?defmods=ok (default) | slow (each step takes 3 s) | fail (the module cannot be uploaded)
const defmods = new URLSearchParams(location.search).get('defmods') ?? 'ok';
setFirmwareEnv({
  ...firmwareEnv(),
  defaultModules: {
    entries: desktopDefaultModules.entries,
    async install(where, _device, ids, progress) {
      const wait = (ms: number) => new Promise((r) => setTimeout(r, defmods === 'slow' ? ms * 4 : ms));
      progress({ text: 'Waiting for the runtime to answer', done: 0, total: ids.length + 2 });
      await wait(700);
      for (const [i, id] of ids.entries()) {
        progress({ text: `Installing ${id} (${i + 1} of ${ids.length})`, done: i + 1, total: ids.length + 2 });
        await wait(900);
        if (defmods === 'fail') throw new DefaultModuleError(id, 'E_NO_SPACE: the card has no room for it');
      }
      progress({
        text: `Enabling ${ids.join(', ')}: restarting the runtime`,
        done: ids.length + 1,
        total: ids.length + 2,
      });
      await wait(1200);
      return [...ids.map((id) => `${id}: uploaded`), `enabled ${ids.join(', ')} (restarted once, ${where})`];
    },
  },
});

installNativeFeel();
setMode(sim.mode);
setLocalFolder(sim.folder === 'none' ? '' : SAMPLES_ROOT);
(window as unknown as { __sim: typeof control }).__sim = control;

render(
  () => (
    <div class="console" style={{ '--accent': panels.dn2.accent, '--func-legend': panels.dn2.func_legend }}>
      <TitleBar />
      <div class="shell">
        <nav class="sidebar" aria-label="Pages">
          <button type="button" class="side toggle" aria-label="Collapse the sidebar">
            <span>«</span>
          </button>
          <span class="side-group">Run</span>
          <button type="button" class={page === 'emulator' ? 'side on' : 'side'}>
            <span>Emulator</span>
          </button>
          <span class="side-group">Device</span>
          <button type="button" class={page === 'transfer' ? 'side on' : 'side'}>
            <span>Transfer</span>
          </button>
          <button type="button" class={page === 'remote' ? 'side on' : 'side'}>
            <span>Remote</span>
          </button>
          <button type="button" class={page === 'firmware' ? 'side on' : 'side'}>
            <span>Firmware</span>
          </button>
        </nav>
        <div class="content">
          {page === 'log' ? (
            <p class="note" style={{ padding: '24px' }}>
              Log preview ({logMode}): the drawer below follows the fake emulator and the fake device.
            </p>
          ) : page === 'remote' ? (
            <RemotePage fallbackDevice="dn2" />
          ) : page === 'emulator' ? (
            <div class="emu-page">
              {/* the header of the Emulator page in ../src/Console.tsx: the file, the runtime option and the button groups */}
              <header class="page-head emu-head">
                <h2>Emulator</h2>
                <span class="file-chip">
                  <span class="path">{bootState ? bootFile : 'Digitone_II_OS1.12.syx'}</span>
                  {bootState ? null : <span class="tag device">dn2 1.12</span>}
                  {bootState ? null : (
                    <span class="tag watch">
                      <i />
                      watching
                    </span>
                  )}
                </span>
                <span class="sp" />
                <label class="tag opt">
                  <input type="checkbox" checked />
                  <span class="long">Boot with runtime</span>
                  {bootState ? null : <span class="rt on">core/dynreg</span>}
                </label>
                <span class="tb-group">
                  <button type="button" class="tb">
                    <span class="lbl">Open…</span>
                  </button>
                  <DefaultModules device="dn2" note="Uploaded and enabled in the emulator after the firmware loads." />
                </span>
                <span class="tb-group">
                  <button type="button" class="tb">
                    <span class="lbl">Reload</span>
                    <kbd class="lbl">⌘R</kbd>
                  </button>
                  <button type="button" class="tb">
                    <span class="lbl">Modules</span>
                    <kbd class="lbl">⇧⌘R</kbd>
                  </button>
                </span>
              </header>
              {emulatorFaceplate()}
            </div>
          ) : page === 'firmware' ? (
            <FirmwarePage />
          ) : (
            <TransferPage />
          )}
        </div>
      </div>
      {page === 'log' ? <LogPage /> : null}
      {page === 'emulator' && bootState ? (
        <>
          {bootState === 'failed' ? (
            <p class="alert" role="alert">
              {bootError}
            </p>
          ) : null}
          <footer class="statusbar">
            <span>
              <i class="dot paused" />
              {bootState === 'loading' ? bootingStatus(startBoot(bootFile)) : 'no firmware'}
            </span>
            <span class="sp" />
            {bootState === 'failed' ? <span class="muted">{loadFailure(bootFile, bootError)}</span> : null}
            <button type="button" class="sb">
              Log
            </button>
            <button type="button" class="sb">
              Metrics
            </button>
          </footer>
        </>
      ) : null}
    </div>
  ),
  document.getElementById('app')!,
);
