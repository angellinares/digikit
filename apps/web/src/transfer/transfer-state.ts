// The Transfer page's state (Modules mode and the mode), kept for the life of the window: closing the page and opening it again shows the last list at once
// (it is refreshed behind it), and an install's verify results stay until the next restart.
import { createSignal } from 'solid-js';
import { transferEnv } from './env';
import { parseMode, type Mode, type Progress, type TransferView, type Verified } from './transfer-core';

export interface DeviceInfo {
  name: string;
  runtime: string;
  free?: number;
  /** Modules mode: the storage line under the device line (storageLine); the free space is in it, not in the device line. */
  storage?: string;
  /** Samples mode only: the backend of the device's samples, and whether it makes folders. */
  samples?: string;
  canMkdir?: boolean;
}

export const [lastView, setLastView] = createSignal<TransferView>();
/** The library modules ticked for install. */
export const [ticks, setTicks] = createSignal<Set<string>>(new Set());
/** The store paths ticked in the On device pane (for Delete). */
export const [delTicks, setDelTicks] = createSignal<Set<string>>(new Set());
export const [verified, setVerified] = createSignal<Verified>({});
export const [progress, setProgress] = createSignal<Progress>();
export const [showTests, setShowTests] = createSignal(false);
export const [log, setLog] = createSignal<string[]>([]);

/** A value kept across restarts of the app in localStorage (a private window or blocked storage: it lasts until the window closes). */
export function remembered<T extends string>(key: string, read: (v: string | null) => T): [() => T, (v: T) => void] {
  let first: string | null = null;
  try {
    first = localStorage.getItem(key);
  } catch {
    // nothing remembered
  }
  const [value, set] = createSignal<T>(read(first));
  return [
    value,
    (v) => {
      set(() => v);
      try {
        if (v) localStorage.setItem(key, v);
        else localStorage.removeItem(key);
      } catch {
        // the choice lasts until the window closes
      }
    },
  ];
}

/** The mode of the page (Modules or Samples). */
export const [mode, setMode] = remembered('digikit.transferMode', parseMode);
/** The mode in use: Samples only where the app gives it (the desktop app). */
export const activeMode = (): Mode => (mode() === 'samples' && transferEnv().extras?.samples ? 'samples' : 'modules');
/** What the On device column says about the device: its name (the profile's), the runtime, the free space of what the mode shows, and the
 * samples backend; `canMkdir` is known after the samples backend was opened. */
export const [deviceInfo, setDeviceInfo] = createSignal<DeviceInfo>();

/** The module whose source the side panel shows (SourcePanel.tsx), or none. */
export const [sourceId, setSourceId] = createSignal<string>();

/** The smoothed speed of the running transfer in bytes per second (the bottom bar sets it; the rows of the file being sent show it). */
export const [liveRate, setLiveRate] = createSignal<number>();

/** The page's one busy action ('' when none) and its error line. They live here, not in the page: leaving the page unmounts it while a job
 * goes on, and the page that comes back must see the job (the buttons stay off, no second job, no read) and show its error. */
export const [busy, setBusy] = createSignal('');
export const [error, setError] = createSignal('');
let readAgain = false;
let stopAsked = false;

/** Start the one busy action `what`; false when another job runs (a read asked for then runs when that job ends: see `endJob`). */
export function beginJob(what: string): boolean {
  if (busy()) {
    if (what === 'Reading') readAgain = true;
    return false;
  }
  setBusy(what);
  stopAsked = false;
  return true;
}

/** The user asked to stop the running job (its Cancel button): a job that loops over files checks this before the next one. */
export const askStop = (): void => {
  stopAsked = true;
};
export const stopWasAsked = (): boolean => stopAsked;

/** End the busy action; true when a read was asked for during it. */
export function endJob(): boolean {
  setBusy('');
  const again = readAgain;
  readAgain = false;
  return again;
}
