// The state of the Firmware page for the life of the window, and the actions on it (the page is FirmwarePage.tsx; the rules are in
// firmware-core.ts, the run in firmware-flow.ts). Signals, so the desktop app keeps a half-finished flash screen while another page is shown.
import { createSignal } from 'solid-js';
import { asked, noteConnection } from '../transfer/connection-state';
import { transferEnv } from '../transfer/env';
import { target } from '../transfer/target';
import { installAfterFlash, patchSet, resetPostRun, postWorking, setCustomBootScreen } from './default-modules-state';
import { firmwareEnv } from './env';
import {
  DEVICE_NAMES,
  IDLE,
  imageFileName,
  isDeviceId,
  modelOfPort,
  refusalText,
  running,
  type DeviceFacts,
  type DeviceState,
  type FlashRoute,
  type FlashState,
  type PrepareState,
} from './firmware-core';
import { runFlash, runRpcFlash } from './firmware-flow';
import { askDevice, portOps, rpcPortOps } from './firmware-ops';

const MAX_BYTES = 16 * 1024 * 1024;

export const [prepared, setPrepared] = createSignal<PrepareState>({ kind: 'none' });
export const [restore, setRestoreSignal] = createSignal(false);
export const [device, setDevice] = createSignal<DeviceState>({ kind: 'checking' });
export const [ack, setAck] = createSignal(false);
export const [run, setRun] = createSignal<FlashState>(IDLE);
export const [notice, setNotice] = createSignal('');
/** How the image gets to the device: the MidiRpc route (default), or the old OS UPGRADE menu route (a small fallback on the page). */
export const [route, setRouteSignal] = createSignal<FlashRoute>('rpc');

export function setRoute(next: FlashRoute): void {
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
    const p = await firmwareEnv().prepare(bytes, restore(), patchSet());
    if (mine !== preparing) return;
    setPrepared({ kind: 'ready', name, size: bytes.length, p });
  } catch (e) {
    if (mine !== preparing) return;
    setPrepared({ kind: 'error', name, message: refusalText(e instanceof Error ? e.message : String(e)) });
  }
}

/** The user picked (or dropped) a file. */
export async function chooseFile(picked: File | undefined): Promise<void> {
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

export function setRestore(on: boolean): void {
  if (running(run())) return;
  setRestoreSignal(on);
  setRun(IDLE);
  void prepare();
}

/** The Custom boot screen toggle: the patch gets core/introblank (or not) and the file is prepared again. */
export function setCustomBoot(on: boolean): void {
  if (running(run())) return;
  setCustomBootScreen(on);
  setRun(IDLE);
  void prepare();
}

/** Look at the selected port: the model, the OS version, the runtime. Feeds the connection indicator too. */
export async function probeDevice(): Promise<void> {
  if (running(run())) return;
  const t = target();
  if (t.kind !== 'midi') {
    const why =
      t.kind === 'emulator'
        ? 'The emulator is not flashed from this page. Pick the MIDI port of a device under Target.'
        : ((await transferEnv().targets.whyNone?.()) ??
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
export function forgetDevice(): void {
  setDevice({ kind: 'checking' });
  setAck(false);
}

let controller: AbortController | undefined;

/** Send the image. The caller has checked `canFlash`. */
export async function startFlash(): Promise<void> {
  const prep = prepared();
  const t = target();
  if (prep.kind !== 'ready' || t.kind !== 'midi' || running(run())) return;
  const p = prep.p;
  const log = firmwareEnv().log;
  controller = new AbortController();
  const what = p.restore ? 'restore' : 'flash';
  const rpc = route() === 'rpc';
  log({
    level: 'info',
    text: `${what} ${p.device} OS ${p.os} to ${t.port}: ${rpc ? `${p.image.length} bytes (MidiRpc)` : `${p.ends.length} messages (OS UPGRADE menu)`}`,
  });
  setNotice('');
  resetPostRun();
  let final: FlashState = IDLE;
  try {
    final = rpc
      ? await runRpcFlash(rpcPortOps(), p, controller.signal, setRun)
      : await runFlash(portOps(), p, controller.signal, setRun);
  } catch (e) {
    final = { phase: 'failed', code: 'send', message: e instanceof Error ? e.message : String(e) };
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
  if (final.phase === 'done' && !p.restore) await installAfterFlash(p.device);
}

export function cancelFlash(): void {
  controller?.abort();
}

/** Back to the start after a stopped or finished run. */
export function resetRun(): void {
  if (running(run()) || postWorking()) return;
  setRun(IDLE);
  resetPostRun();
}

/** Give the user the image as a file. */
export async function downloadImage(): Promise<void> {
  const prep = prepared();
  if (prep.kind !== 'ready') return;
  try {
    const where = await firmwareEnv().saveImage(imageFileName(prep.p), prep.p.image);
    setNotice(where ? `Saved ${where}` : '');
    if (where) firmwareEnv().log({ level: 'info', text: `saved the image: ${where}` });
  } catch (e) {
    setNotice(`Could not save the file: ${e instanceof Error ? e.message : String(e)}`);
  }
}

/** The site: open the patched image in the emulator page. */
export async function runInEmulator(): Promise<void> {
  const prep = prepared();
  const open = firmwareEnv().runInEmulator;
  if (prep.kind !== 'ready' || !open) return;
  try {
    await open(imageFileName(prep.p), prep.p.device, prep.p.image);
  } catch (e) {
    setNotice(`Could not pass the file to the emulator: ${e instanceof Error ? e.message : String(e)}`);
  }
}
