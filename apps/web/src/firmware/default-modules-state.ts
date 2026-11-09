// The state of the Default modules list for the life of the window: which entries the user unticked (kept in localStorage), and the run that
// follows a flash (the part of the Firmware page that shows it is FirmwarePage.tsx; the emulator's run is Console.tsx). The rules are in
// default-modules-core.ts; the app gives the entries and the install through `FirmwareEnv.defaultModules`.
import { createSignal } from 'solid-js';
import {
  DefaultModuleError,
  parseUnticked,
  patchSetFor,
  tickedIds,
  withTick,
  type DefaultModuleEntry,
  type Unticked,
} from './default-modules-core';
import { firmwareEnv } from './env';

const KEY = 'digikit.defaultModules.unticked';

function read(): Unticked {
  try {
    return parseUnticked(localStorage.getItem(KEY));
  } catch {
    return {};
  }
}

export const [unticked, setUnticked] = createSignal<Unticked>(read());

const BOOT_KEY = 'digikit.customBootScreen';

function readBoot(): boolean {
  try {
    return localStorage.getItem(BOOT_KEY) === '1';
  } catch {
    return false;
  }
}

/** The Custom boot screen toggle (core/introblank in the patch roots), off by default; kept in localStorage. */
export const [customBootScreen, setCustomBootScreenSignal] = createSignal<boolean>(readBoot());

export function setCustomBootScreen(on: boolean): void {
  setCustomBootScreenSignal(on);
  try {
    localStorage.setItem(BOOT_KEY, on ? '1' : '0');
  } catch {
    // the choice is not kept
  }
}

/** The patch set the page prepares: `device`, or `device-introblank` with the toggle on. */
export const patchSet = (): string => patchSetFor(customBootScreen());

export function setTick(device: string, id: string, on: boolean): void {
  const next = withTick(unticked(), device, id, on);
  setUnticked(next);
  try {
    localStorage.setItem(KEY, JSON.stringify(next));
  } catch {
    // the choice is not kept
  }
}

/** The list for a device, or none where the app gives no default modules. */
export const entriesFor = (device: string): DefaultModuleEntry[] => firmwareEnv().defaultModules?.entries(device) ?? [];

/** The ids ticked for the device, in list order. */
export const tickedFor = (device: string): string[] => tickedIds(entriesFor(device), unticked(), device);

/** The run of the default modules after a flash. */
export type PostRun =
  | { kind: 'idle' }
  | { kind: 'working'; text: string; done: number; total: number }
  | { kind: 'done'; lines: string[] }
  | { kind: 'failed'; message: string };

export const [postRun, setPostRun] = createSignal<PostRun>({ kind: 'idle' });
export const postWorking = (): boolean => postRun().kind === 'working';

/**
 * After a finished flash: install and enable the ticked default modules of the device on the flashed device. One run, no question: the flash
 * stays done when a module fails, and the failed module is named. Does nothing where the app has no default modules or nothing is ticked.
 */
export async function installAfterFlash(device: string): Promise<void> {
  const env = firmwareEnv();
  const ids = tickedFor(device);
  if (!env.defaultModules || ids.length === 0) return;
  setPostRun({ kind: 'working', text: 'Starting the default modules', done: 0, total: ids.length + 2 });
  env.log({ level: 'info', text: `default modules for ${device}: ${ids.join(', ')}` });
  try {
    const lines = await env.defaultModules.install('device', device, ids, (p) => setPostRun({ kind: 'working', ...p }));
    setPostRun({ kind: 'done', lines });
    env.log({ level: 'info', text: 'default modules installed', detail: lines.join('\n') });
  } catch (e) {
    const message =
      e instanceof DefaultModuleError
        ? `Default module ${e.message}. The firmware flash is done; install it on the Transfer page.`
        : `Default modules: ${e instanceof Error ? e.message : String(e)}. The firmware flash is done.`;
    setPostRun({ kind: 'failed', message });
    env.log({ level: 'error', text: message });
  }
}

export const resetPostRun = (): void => {
  if (!postWorking()) setPostRun({ kind: 'idle' });
};
