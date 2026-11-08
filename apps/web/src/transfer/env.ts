// What the Transfer pane takes from the app it runs in. The pane (TransferPage.tsx and the files beside it) is the same code in the
// desktop workbench and on the site; everything that differs is behind this interface. The app builds one `TransferEnv` and gives it to
// `setTransferEnv` before it shows the pane (or the target selector of another page).
//   desktop: apps/desktop/ui/src/desktop-env.ts (Tauri commands: the library folder, MIDI ports, the emulator, samples, the card)
//   site:    apps/web/src/transfer/web-env.ts (WebMIDI, the built modules the site ships, their sources)
import type { JSX } from 'solid-js';
import type { DkTransport } from '@digi/dk-session';
import type { Target } from './target';
import type { BarInfo } from './transfer-core';
import type { PageCtx } from './TransferParts';

/** A built module file of the library: the path inside the device's folder and its size. */
export interface LibraryFile {
  path: string;
  size: number;
}

/** The modules the app can offer for install, per device id (`dt2`, `dn2`). */
export interface TransferLibrary {
  /** The .dkm files of the device; read again at each call (a rebuild shows up at the next read). */
  scan(device: string): Promise<LibraryFile[]>;
  /** The bytes of one file of `scan`. */
  read(device: string, path: string): Promise<Uint8Array>;
  /**
   * Files the user adds (the "Add .dkm" button, a drop): the site keeps them in the library list, to install from there. The desktop leaves it out;
   * its Add puts the files on the card at once. Throws a readable error for a file that is not a .dkm.
   */
  addFiles?(files: { name: string; data: Uint8Array }[]): Promise<string[]>;
}

/** The read-only sources of the library's modules (the site ships them; the desktop's library has none). */
export interface TransferSource {
  /** The source files of a module (paths relative to its folder, `module.toml` first), or none when the site has no source of it. */
  files(id: string): string[];
  /** The text of one of them. */
  read(id: string, path: string): Promise<string>;
}

/** The `[files]` table of a module.toml: the files the module owns under `data/<module id>/` and how the page shows them. */
export interface ModuleFilesSpec {
  /** The title of the files area. */
  label: string;
  /** The extensions of the files, without the dot. */
  ext: string[];
  /** The key of the preview in ModuleFilePreview.tsx (`oled-1bit`). */
  preview: string;
}

/** The targets the user can pick: a MIDI port with a device on it, and where the app has one, the emulator. */
export interface TransferTargets {
  /** The app has an emulator target (the site has not: its first target is a MIDI port, or none until one is there). */
  emulator: boolean;
  /** The names of the MIDI ports that a device can be on. */
  ports(): Promise<string[]>;
  /** Open the target (a MIDI port is opened here) and give its transport. Called once per use of the target. */
  open(target: Target): Promise<DkTransport>;
  /** Let go of a MIDI port that is no longer the target. */
  close?(target: Target): Promise<void>;
  /** The text of a request on the `none` target: why there is no port (the site: no Web MIDI, no permission), and what to do. Asks again when it can. */
  whyNone?(): Promise<string>;
  /** Called before each request of a target; it throws when the target cannot answer now (a paused emulator). */
  preflight?(target: Target): Promise<void>;
}

/** One run of the pane (Installing, Deleting, ...), for the app's log. */
export interface TransferRun {
  /** The page's busy label: Installing, Restarting, Uploading, Deleting, Creating. */
  what: string;
  target: string;
  /** The error text of a failed run. */
  error?: string;
  cancelled?: boolean;
  /** A cancelled upload run: what it did (`87 of 357 uploaded, 270 not sent`). */
  counts?: string;
  /** The text the progress bar ended with. */
  message?: string;
  /** The events of the run (Modules mode), one line each. */
  lines: string[];
}

/** The Samples mode, which only the desktop app has (it needs the folders of the computer). The pane shows the mode only when this is given. */
export interface SamplesSlot {
  /** The body of the mode: both columns. */
  Mode: (props: { ctx: PageCtx }) => JSX.Element;
  /** Read both sides of the mode (throws when either fails). */
  read(): Promise<void>;
  /** Another target: forget what was read. */
  reset(): void;
  /** The bar along the bottom for a running or finished upload. */
  bar(busy: string): { info: BarInfo; running: boolean } | undefined;
  /** One line per file of the last upload run, for the Log. */
  lines?(): string[];
  /** The light poll of the chosen folder: true when its listing changed (and the pane reads again). */
  localChanged(): Promise<boolean>;
}

export interface TransferEnv {
  library: TransferLibrary;
  /** The bytes of patchkit.wasm (its `pk_needs` tells what a .dkm takes of the device). */
  patchkitWasm(): Promise<ArrayBuffer>;
  /** The name of the device with this id (`dt2`: "Digitakt II"). */
  deviceName(device: string): Promise<string>;
  targets: TransferTargets;
  /** The Source link of a library row and its panel; the pane shows them only when this is given. */
  source?: TransferSource;
  /** The `[files]` table of the module with this id, or none; the pane shows a files area only for a module that has one. */
  moduleFiles?(id: string): ModuleFilesSpec | undefined;
  /** The app's log of what happened (a run that ended, with its result). */
  logRun(run: TransferRun): void;
  /** Only the desktop app has these; the pane shows a part only when it is given. */
  extras?: {
    samples?: SamplesSlot;
    /** A job ended (the Card panel reads the folder now and again after its quiet period). */
    onJobEnd?(): void;
  };
}

let current: TransferEnv | undefined;

export function setTransferEnv(env: TransferEnv): void {
  current = env;
}

export function transferEnv(): TransferEnv {
  if (!current) throw new Error('The Transfer pane has no environment: call setTransferEnv first.');
  return current;
}
