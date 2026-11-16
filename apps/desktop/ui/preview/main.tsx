// The preview entry: the desktop shell (title bar, sidebar, content) around the real Transfer page, over the fake device of sim.ts.
// Mirrors the markup of ../src/Console.tsx for the parts around the page, so widths and heights are the app's.
import { render } from 'solid-js/web';
import '@fontsource-variable/archivo/wdth.css';
import '@fontsource/jetbrains-mono/400.css';
import '@fontsource/jetbrains-mono/500.css';
import '@digi/workbench-core/ui/styles/site.css';
import '../src/console.css';
import '../src/native.css';
import '../src/tooltip.css';
import { createDesktopApps } from '../src/desktop-app';
import { createTauriHost } from '../src/host/tauri-host';
import { CHART, FOLDER, Icon, MODULES, PAUSE, RELOAD, RESTART } from '../src/icons';
import { installNativeFeel } from '../src/native';
import { installTooltips } from '../src/tooltip';
import DefaultModules from '@digi/workbench-core/ui/firmware/DefaultModules.tsx';
import { DefaultModuleError } from '@digi/workbench-core/firmware/default-modules-core.ts';
import { createFirmwareApp } from '@digi/workbench-core/firmware/app.ts';
import AppProvider from '@digi/workbench-core/ui/transfer/AppProvider.tsx';
import LivePanel from '@digi/workbench-core/ui/components/LivePanel.tsx';
import { panels } from '@digi/workbench-core/data/faceplates.ts';
import FirmwarePage from '@digi/workbench-core/ui/firmware/FirmwarePage.tsx';
import RemotePage from '../src/RemotePage';
import SidePanels from '../src/SidePanels';
import TitleBar from '../src/TitleBar';
import { setLocalFolder } from '../src/samples-state';
import TransferPage from '@digi/workbench-core/ui/transfer/TransferPage.tsx';
import FilesPage from '@digi/workbench-core/ui/files/FilesPage.tsx';
import { demoStore } from '@digi/workbench-core/target-ops/fake-store.ts';
import { demoLibrary } from './files-mock';
import { deviceFromName, bootingStatus, booting as startBoot, loadFailure } from '../src/boot-core';
import { cardDevice, reloadCard } from './card-mock';
import CardPanel from '../src/CardPanel';
import { installRemoteMock, testPattern } from './remote-mock';
import { queryFrame } from './frame-from-png';
import { createLogFollow } from '../src/log-follow';
import LogPanel from '../src/LogPanel';
import { setLogOpen } from '../src/log-state';
import { logMode, previewLog } from './runtime-log-mock';
import { previewNames } from './runtime-names-mock';
import { SAMPLES_ROOT, bindSimApp, control, sim } from './sim';
const host = createTauriHost();
const apps = createDesktopApps(host);
bindSimApp(apps.app);

// ?page=transfer (default) | files | remote | emulator | firmware (state: fwdev, fwstate, fwos; see firmware-mock.ts): the Emulator page shows the faceplate only (the real one needs the emulator) with the same
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
      <SidePanels
        host={host}
        visible
        active={false}
        metrics={
          <div class="card-body metrics">
            <p class="poll">1 s poll</p>
            <p class="note">The emulator is not running: no readings yet.</p>
          </div>
        }
        session={() => Promise.resolve({} as never)}
        where={() => 'the emulator'}
      />
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
        host={host}
        visible
        metrics={
          // the Metrics tab of ../src/Console.tsx with the values of a running DN2
          <div class="card-body metrics">
            <p class="poll">1 s poll</p>
            <dl>
              <dt>Firmware</dt>
              <dd>dn2 1.12 · running</dd>
              <dt>Audio output</dt>
              <dd>MacBook Pro Speakers</dd>
              <dt>Speed</dt>
              <dd>1.00× real time (recent)</dd>
              <dt>Underruns</dt>
              <dd>0</dd>
              <dt>CF load</dt>
              <dd>38 %</dd>
              <dt>DSP load (est.)</dt>
              <dd>52 % est. (worst 61 %), model 47 %</dd>
            </dl>
          </div>
        }
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
    emulator: { running: previewLog.emulatorRunning, session: async () => previewLog.emulator, names: previewNames },
    device: { port: previewLog.devicePort, session: async () => previewLog.device, present: async () => true },
  });
  return (
    <section
      class="log-drawer"
      aria-label="Log"
      style={{ height: `${Number(new URLSearchParams(location.search).get('logh') ?? 360)}px` }}
    >
      <LogPanel
        host={host}
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
const firmware = createFirmwareApp(
  {
    ...apps.firmware.env,
    defaultModules: {
      entries: apps.firmware.env.defaultModules!.entries,
      async install(where, _device, ids, progress, _opts) {
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
  },
  apps.app,
);

const filesStore = demoStore();
// ?page=files&card=dn2|dt2 shows the emulator card-folder strip (preview/card-mock.ts) above the tree
const showCard = new URLSearchParams(location.search).has('card');

installNativeFeel();
installTooltips();
apps.app.state.setMode(sim.mode);
setLocalFolder(sim.folder === 'none' ? '' : SAMPLES_ROOT);
(window as unknown as { __sim: typeof control }).__sim = control;

render(
  () => (
    <AppProvider app={apps.app} firmware={firmware}>
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
            <button type="button" class={page === 'files' ? 'side on' : 'side'}>
              <span>Files</span>
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
              <RemotePage fallbackDevice="dn2" emulatorLoaded={true} />
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
                    <button type="button" class="tb" aria-label="Open firmware file" title="Open a firmware file">
                      <Icon d={FOLDER} />
                      <span class="lbl">Open…</span>
                    </button>
                    <DefaultModules
                      device="dn2"
                      note="Uploaded and enabled in the emulator after the firmware loads."
                    />
                  </span>
                  <span class="tb-group">
                    <button type="button" class="tb" aria-label="Reload" title="Reload the startup file (⌘R)">
                      <Icon d={RELOAD} />
                      <span class="lbl">Reload</span>
                      <kbd class="lbl">⌘R</kbd>
                    </button>
                    <button
                      type="button"
                      class="tb"
                      disabled={!!bootState}
                      aria-label="Reload modules"
                      title="Build the stale module objects, then apply the modules (⇧⌘R)"
                    >
                      <Icon d={MODULES} />
                      <span class="lbl">Modules</span>
                      <kbd class="lbl">⇧⌘R</kbd>
                    </button>
                  </span>
                  {/* no status while booting: the three buttons are disabled, as in the app */}
                  <span class="tb-group">
                    <button
                      type="button"
                      class="tb icon"
                      disabled={!!bootState}
                      aria-label="Pause"
                      title="Pause the emulator"
                    >
                      <Icon d={PAUSE} />
                    </button>
                    <button
                      type="button"
                      class="tb icon"
                      disabled={!!bootState}
                      aria-label="Reset (emulator)"
                      title="Reset the running firmware to its ready state"
                    >
                      <Icon d={RESTART} />
                    </button>
                    <button
                      type="button"
                      class="tb icon"
                      disabled={!!bootState}
                      aria-label="Save diagnostics"
                      title="Save diagnostics"
                    >
                      <Icon d={CHART} />
                    </button>
                  </span>
                </header>
                {emulatorFaceplate()}
              </div>
            ) : page === 'firmware' ? (
              <FirmwarePage />
            ) : page === 'files' ? (
              <FilesPage
                store={filesStore}
                library={() => demoLibrary(filesStore)}
                save={async (name) => `/tmp/${name}`}
                extra={showCard ? <CardPanel host={host} reload={reloadCard} /> : undefined}
              />
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
    </AppProvider>
  ),
  document.getElementById('app')!,
);
