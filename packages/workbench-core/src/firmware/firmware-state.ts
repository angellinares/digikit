// The state of the Firmware page for the life of the window, and the actions on it (the page is FirmwarePage.tsx; the rules are in
// firmware-core.ts, the run in firmware-flow.ts). Signals, so the desktop app keeps a half-finished flash screen while another page is shown.
import { createSignal } from 'solid-js';
import { JobBusyError } from '../jobs/jobs';
import type { TransferApp } from '../transfer/app';
import type { DefaultModulesApp } from './default-modules-state';
import type { FirmwareEnv } from './env';
import {
  DEVICE_NAMES,
  IDLE,
  imageFileName,
  isDeviceId,
  modelOfPort,
  progressView,
  refusalText,
  running,
  type DeviceFacts,
  type DeviceState,
  type FlashRoute,
  type FlashState,
  type PrepareState,
} from './firmware-core';
import { runFlash, runRpcFlash } from './firmware-flow';
import { createFirmwareOps } from './firmware-ops';

const MAX_BYTES = 16 * 1024 * 1024;

export function createFirmwareState({
  env,
  transfer,
  defaults,
}: {
  env: FirmwareEnv;
  transfer: TransferApp;
  defaults: DefaultModulesApp;
}) {
  const { target } = transfer.target;
  const { asked, noteConnection } = transfer.connection;
  const { askDevice, portOps, rpcPortOps } = createFirmwareOps(env, transfer);
  const [prepared, setPrepared] = createSignal<PrepareState>({ kind: 'none' });
  const [restore, setRestoreSignal] = createSignal(false);
  const [device, setDevice] = createSignal<DeviceState>({ kind: 'checking' });
  const [ack, setAck] = createSignal(false);
  const [run, setRun] = createSignal<FlashState>(IDLE);
  const [notice, setNotice] = createSignal('');
  /** How the image gets to the device: the MidiRpc route (default), or the old OS UPGRADE menu route (a small fallback on the page). */
  const [route, setRouteSignal] = createSignal<FlashRoute>('rpc');

  function setRoute(next: FlashRoute): void {
    if (running(run())) return;
    setRouteSignal(next);
    setRun(IDLE);
    setAck(false);
  }

  let file: { name: string; bytes: Uint8Array } | undefined;
  let preparing = 0;

  /** Patch and check the picked file (again for another mode). A newer request makes the older answer unused. */
  async function prepare(): Promise<void> {
    if (!file) return;
    const mine = ++preparing;
    const { name, bytes } = file;
    setPrepared({ kind: 'working', name });
    try {
      const p = await env.prepare(bytes, restore(), defaults.patchSet());
      // a stock image patchkit knows is kept for binding services (Transfer); a failed write is not a failed prepare
      void transfer.stock.remember(p.device, p.shaStock, bytes).catch(() => {});
      if (mine !== preparing) return;
      setPrepared({ kind: 'ready', name, size: bytes.length, p });
    } catch (e) {
      if (mine !== preparing) return;
      setPrepared({ kind: 'error', name, message: refusalText(e instanceof Error ? e.message : String(e)) });
    }
  }

  /** The user picked (or dropped) a file. */
  async function chooseFile(picked: File | undefined): Promise<void> {
    if (!picked || running(run())) return;
    setRun(IDLE);
    setAck(false);
    if (picked.size > MAX_BYTES) {
      file = undefined;
      setPrepared({
        kind: 'error',
        name: picked.name,
        message: 'That file is too large for a firmware update (the limit is 16 MB).',
      });
      return;
    }
    file = { name: picked.name, bytes: new Uint8Array(await picked.arrayBuffer()) };
    await prepare();
  }

  function setRestore(on: boolean): void {
    if (running(run())) return;
    setRestoreSignal(on);
    setRun(IDLE);
    void prepare();
  }

  /** The Custom boot screen toggle: the patch gets core/introblank (or not) and the file is prepared again. */
  function setCustomBoot(on: boolean): void {
    if (running(run())) return;
    defaults.setCustomBootScreen(on);
    setRun(IDLE);
    void prepare();
  }

  /** Look at the selected port: the model, the OS version, the runtime. Feeds the connection indicator too. */
  async function probeDevice(): Promise<void> {
    if (running(run())) return;
    const t = target();
    if (t.kind !== 'midi') {
      const why =
        t.kind === 'emulator'
          ? 'The emulator is not flashed from this page. Pick the MIDI port of a device under Target.'
          : ((await transfer.env.targets.whyNone?.()) ??
            'No MIDI port. Connect the device by USB, set its USB mode to MIDI, then pick it under Target.');
      setDevice({ kind: 'none', why });
      noteConnection({ type: 'lost', reason: why });
      return;
    }
    if (device().kind !== 'connected') setDevice({ kind: 'checking' });
    try {
      const a = await asked(() => askDevice());
      if (running(run()) || target().kind !== 'midi') return;
      const model = isDeviceId(a.model ?? '') ? (a.model as DeviceFacts['model']) : modelOfPort(a.port);
      const facts: DeviceFacts = {
        port: a.port,
        model,
        ...(a.os ? { os: a.os } : {}),
        ...(a.runtime ? { runtime: a.runtime } : {}),
        ...(a.imageId !== undefined ? { imageId: a.imageId } : {}),
      };
      setDevice({ kind: 'connected', d: facts });
      noteConnection({
        type: 'answered',
        device: {
          id: model ?? '',
          name: model ? DEVICE_NAMES[model] : 'MIDI device',
          ...(a.os ? { os: a.os } : {}),
          ...(a.runtime ? { runtime: a.runtime } : {}),
        },
      });
    } catch (e) {
      if (running(run())) return;
      setDevice({ kind: 'lost', reason: e instanceof Error ? e.message.split('\n')[0]! : String(e) });
    }
  }

  /** The target changed: the old reading is not shown. */
  function forgetDevice(): void {
    setDevice({ kind: 'checking' });
    setAck(false);
  }

  let controller: AbortController | undefined;

  /** Send the image. The caller has checked `canFlash`. */
  async function startFlash(): Promise<void> {
    const prep = prepared();
    const t = target();
    if (prep.kind !== 'ready' || t.kind !== 'midi' || running(run())) return;
    const p = prep.p;
    const log = env.log;
    const what = p.restore ? 'restore' : 'flash';
    const rpc = route() === 'rpc';
    let final: FlashState = IDLE;
    try {
      final = await transfer.jobs.runJob('Flashing firmware', { group: 'target', cancel: cancelFlash }, async (job) => {
        controller = new AbortController();
        log({
          level: 'info',
          text: `${what} ${p.device} OS ${p.os} to ${t.port}: ${rpc ? `${p.image.length} bytes (MidiRpc)` : `${p.ends.length} messages (OS UPGRADE menu)`}`,
        });
        setNotice('');
        defaults.resetPostRun();
        const report = (s: FlashState): void => {
          setRun(s);
          if (s.phase === 'sending') {
            const v = progressView(s);
            job.progress(v.fraction, v.text);
          } else job.progress(undefined, s.phase === 'rebooting' ? 'restarting' : undefined);
        };
        try {
          return rpc
            ? await runRpcFlash(rpcPortOps(), p, controller.signal, report)
            : await runFlash(portOps(), p, controller.signal, report);
        } catch (e) {
          const failed: FlashState = {
            phase: 'failed',
            code: 'send',
            message: e instanceof Error ? e.message : String(e),
          };
          setRun(failed);
          return failed;
        }
      });
    } catch (e) {
      if (!(e instanceof JobBusyError)) throw e;
      final = { phase: 'failed', code: 'send', message: 'a transfer is running; wait for it' };
      setRun(final);
    }
    setAck(false);
    // the device is another device now (or the same one, restarted): read it again
    void probeDevice();
    if (final.phase === 'done')
      log({ level: 'info', text: `${what} done: ${final.message}`, detail: final.lines.join('\n') });
    else if (final.phase === 'stopped')
      log({
        level: final.reason === 'error' ? 'error' : 'info',
        text: `${what} stopped after ${final.sent} of ${final.total} ${final.unit ?? 'messages'}`,
        detail: final.message,
      });
    else if (final.phase === 'failed') log({ level: 'error', text: `${what} failed: ${final.message}` });
    // the flash is done and the runtime answered: the default modules follow in the same run (the flash stays done when one fails)
    if (final.phase === 'done' && !p.restore) await defaults.installAfterFlash(p.device);
  }

  function cancelFlash(): void {
    controller?.abort();
  }

  /** Back to the start after a stopped or finished run. */
  function resetRun(): void {
    if (running(run()) || defaults.postWorking()) return;
    setRun(IDLE);
    defaults.resetPostRun();
  }

  /** Give the user the image as a file. */
  async function downloadImage(): Promise<void> {
    const prep = prepared();
    if (prep.kind !== 'ready') return;
    try {
      const where = await env.saveImage(imageFileName(prep.p), prep.p.image);
      setNotice(where ? `Saved ${where}` : '');
      if (where) env.log({ level: 'info', text: `saved the image: ${where}` });
    } catch (e) {
      setNotice(`Could not save the file: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  /** The site: open the patched image in the emulator page. */
  async function runInEmulator(): Promise<void> {
    const prep = prepared();
    const open = env.runInEmulator;
    if (prep.kind !== 'ready' || !open) return;
    try {
      await open(imageFileName(prep.p), prep.p.device, prep.p.image);
    } catch (e) {
      setNotice(`Could not pass the file to the emulator: ${e instanceof Error ? e.message : String(e)}`);
    }
  }

  return {
    prepared,
    setPrepared,
    restore,
    setRestore,
    device,
    ack,
    setAck,
    run,
    notice,
    route,
    setRoute,
    chooseFile,
    setCustomBoot,
    probeDevice,
    forgetDevice,
    startFlash,
    cancelFlash,
    resetRun,
    downloadImage,
    runInEmulator,
  };
}
export type FirmwareState = ReturnType<typeof createFirmwareState>;
