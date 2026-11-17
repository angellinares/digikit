// The Host: every Tauri command the desktop UI calls, as typed methods in groups. The UI modules take a `Host` and never import Tauri; the
// real implementation is tauri-host.ts (the only module that calls `invoke`), tests use fake-host.ts. The Rust side is
// apps/desktop/src-tauri/src/main.rs (`generate_handler!`) and its command modules. The shapes here follow the Rust signatures and
// carry no UI types, so the folder can move into a shared package (the browser app would implement the same interface).

/** The emulator control RPC (`emu_call`): one protocol method of the controller (docs/CONTROL.md). */
export interface EmuRpc {
  /** `session`: the session token of the webview; the controller refuses the call when it is no longer the current one (`firmware.load` and `restart` start the session they carry). */
  call<T>(method: string, params?: Record<string, unknown>, session?: number): Promise<T>;
}

/** A file or folder of a listing of the chosen local folder (`local_list`). */
export interface LocalListing {
  name: string;
  kind: 'file' | 'folder';
  size: number;
}

/** What `lib_build` reports (library.rs). */
export interface LibBuildReport {
  packed: number;
  changed: string[];
  removed: string[];
  failed: { id: string; error: string }[];
  ms: number;
}

/** The firmware file the emulator boots (`emu_set_source`, `emu_source_info`). */
export interface EmuSource {
  path: string;
  name: string;
  bytes: number;
  modified_ms: number;
}

/** What `emu_launch` says about the launch of the app. */
export interface EmuLaunch {
  syx: string | null;
  card: string | null;
  runtime: boolean | null;
  loaded: boolean;
}

/** What `bulk_list` returns: `out_only` marks the OUT-only interface (USB AUDIO/MIDI mode); `id` is `bus:address`. */
export interface BulkListed {
  id: string;
  name: string;
  out_only: boolean;
}

/** Files and folders on the computer. */
export interface FsHost {
  /** The system's folder dialog; null when the user cancels. */
  pickFolder(options: { start: string | null; title?: string }): Promise<string | null>;
  list(root: string, rel: string): Promise<LocalListing[]>;
  /** The bytes of a file below `root`; `head`: only the first bytes. */
  read(root: string, rel: string, head: number | null): Promise<ArrayBuffer>;
  /** The system's save dialog for a log text; the path, or null when the user cancels. `kind` 'file' saves any file (no type filter), used by the Files page. */
  saveLog(bytes: Uint8Array, name: string, kind?: 'log' | 'profile' | 'file'): Promise<string | null>;
}

/** The address map of one built module (docs/ADDRMAP.md): the JSON text `lib_build` wrote beside its `.dkm`. */
export interface LibMap {
  /** The module's library path without `.dkm` (`author/name`). */
  id: string;
  json: string;
}

/** The ship map `firmware_prepare` kept for a prepared image, and the addresses of its load-table symbols (docs/ADDRMAP.md "Workbench"). */
export interface ShipRecord {
  /** The id the image reports in INFO after the flash, 16 hex digits. */
  image_id: string;
  /** The ship map as JSON text. */
  ship: string;
  symbols: Record<string, number>;
}

/** What `lib_maps` answers: the ship maps kept for the device's images, and the maps of its built modules. */
export interface LibMaps {
  ships: ShipRecord[];
  modules: LibMap[];
}

/** The library folder of built modules and the device profiles. */
export interface LibraryHost {
  build(device: string): Promise<LibBuildReport>;
  scan(device: string): Promise<{ path: string; size: number }[]>;
  read(device: string, path: string): Promise<ArrayBuffer>;
  /** The address maps of the device: the ship maps of the images `firmware.prepare` made, and the maps of its built modules (`build` writes them; a module built before has none). */
  readMaps(device: string): Promise<LibMaps>;
  /** What the device profile says about the device with this id (`dt2`): its name and the backend of its samples. */
  deviceStorage(device: string): Promise<{ name: string; samples: string }>;
}

/** The MIDI port the app opens through its Rust side (midir). One port at a time. */
export interface MidiHost {
  ports(): Promise<string[]>;
  open(name: string): Promise<void>;
  close(): Promise<void>;
  send(message: Uint8Array): Promise<void>;
  /** The next message; an empty buffer after `timeoutMs` or after `cancel`. */
  recv(timeoutMs: number): Promise<ArrayBuffer>;
  /** End the pending `recv` now (it answers as if its time had run out). */
  cancel(): Promise<void>;
}

/** The USB bulk interface (nusb). One link at a time; the OUT-only pair has no `recv` (the replies come over SysEx). */
export interface BulkHost {
  list(): Promise<BulkListed[]>;
  open(id: string): Promise<void>;
  close(): Promise<void>;
  send(frame: Uint8Array): Promise<void>;
  recv(timeoutMs: number): Promise<ArrayBuffer>;
  cancel(): Promise<void>;
  openOutOnly(id: string): Promise<void>;
  sendOutOnly(frame: Uint8Array): Promise<void>;
  closeOutOnly(): Promise<void>;
}

export interface PortsHost {
  midi: MidiHost;
  bulk: BulkHost;
}

/** Firmware patching and the wasm of the shared Rust tools. */
export interface FirmwareHost {
  /** Patch `stock` with the checkout's modules: 4 bytes of JSON length (big endian), the summary JSON, the image. */
  prepare(stock: Uint8Array, options: { restore: boolean; set: string }): Promise<ArrayBuffer>;
  /** The system's save dialog for an image; the path, or null when the user cancels. */
  save(bytes: Uint8Array, name: string): Promise<string | null>;
  patchkitWasm(): Promise<ArrayBuffer>;
  /** `modules/bind.json` of the site build (`{ports, fw}`, no firmware): what `pk_bind` reads to bind a service file. */
  bindBundle(): Promise<ArrayBuffer>;
  /** `dkprof.wasm`: address lookup and load table decoding (the Log pane's names). */
  dkprofWasm(): Promise<ArrayBuffer>;
  sampleWasm(): Promise<ArrayBuffer>;
}

/** The emulator app shell: the firmware file, the launch, the build. The RPC to the running emulator is `rpc`. */
export interface EmuHost {
  setSource(path: string): Promise<EmuSource | null>;
  /** The system's file dialog for a firmware file; null when the user cancels. */
  pickSyx(start: string | null): Promise<string | null>;
  sourceInfo(): Promise<EmuSource | null>;
  /** The build of the native emulator (the PGO warning reads it); the shape is the UI's (pgo-warning.ts). */
  buildInfo<T>(): Promise<T>;
  launch(): Promise<EmuLaunch>;
  /** The page runs inside the desktop app (false in a plain browser, such as the preview). */
  inApp(): boolean;
  /** The snapshot of the startup firmware, or a refusal when none is selected (the shapes are the UI's: native-runtime.ts). */
  startup<T>(): Promise<T>;
  /** Run the guest for `budget` instructions of the session; the snapshot after it. */
  step<T>(sessionId: number, budget: number): Promise<T>;
  /** Press a key, release it after `hold` instructions; the snapshot after it. */
  tap<T>(sessionId: number, code: number, hold?: number): Promise<T>;
  stop(sessionId: number): Promise<void>;
  /** The status of the default audio output, or null when there is none yet. */
  audioStatus<T>(sessionId: number): Promise<T | null>;
  /** Save a diagnostics report; the path written. */
  saveDiagnostics(name: string, json: string): Promise<string>;
  /** Put a firmware file the user picked in the page into a file the controller reads; its path. */
  stageSyx(bytes: Uint8Array, name: string): Promise<string>;
  /** The card of the Digitakt II built from files (`CardBridge`). */
  card: {
    reset(): Promise<void>;
    stage(name: string, bytes: Uint8Array): Promise<void>;
    load<T>(name: string | null): Promise<T>;
  };
}

/** The operating system's shell. */
export interface ShellHost {
  /** Open the card folder (`card.folder`) in the file manager; the path comes from the controller, not from the page. */
  openCardFolder(): Promise<void>;
}

export interface Host {
  fs: FsHost;
  library: LibraryHost;
  ports: PortsHost;
  firmware: FirmwareHost;
  emu: EmuHost;
  rpc: EmuRpc;
  shell: ShellHost;
}
