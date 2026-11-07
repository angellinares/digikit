// The desktop workbench's side of the Transfer pane (apps/web/src/transfer/env.ts): the library folder of built modules, the MIDI ports and
// the emulator through the Tauri commands, the samples mode and the Card panel. Loaded first by main.tsx.
import { invoke } from '@tauri-apps/api/core';
import { listingChanged } from '../../../web/src/transfer/refresh-core';
import { setTransferEnv, type SamplesSlot, type TransferEnv } from '../../../web/src/transfer/env';
import { target, targetLabel, type Target } from '../../../web/src/transfer/target';
import { failureLine } from '../../../web/src/transfer/wait-core';
import './desktop-firmware';
import { bumpCardRefresh } from './card-state';
import { EmuUsbTransport, emulatorMode } from './desktop-transport';
import { logInput } from './log-state';
import { transferEntry } from './log-core';
import { closeMidiPort, listMidiPorts, MidiPortTransport, openMidiPort } from './midi-transport';
import { deviceStorage, listLocalNow, readLocal, readSamples } from './samples-backend';
import { samplesBar, uploadLines } from './samples-core';
import {
  localEntries,
  localFolder,
  setSampleEntries,
  setSampleFolder,
  setSampleTicks,
  setUploads,
  uploadBase,
  uploadAverage,
  uploads,
} from './samples-state';
import SamplesMode from './SamplesMode';

export const PAUSED =
  'The emulator is paused: nothing would run the transfer. Resume it (the Run button of the workbench), then try again.';

const samples: SamplesSlot = {
  Mode: SamplesMode,
  async read() {
    // the device and the folder fail apart: both are read, the errors are shown together
    const errors: string[] = [];
    for (const step of [readSamples, readLocal])
      try {
        await step();
      } catch (e) {
        errors.push(failureLine(e, targetLabel(target())));
      }
    if (errors.length) throw new Error(errors.join('\n'));
  },
  reset() {
    setSampleEntries(undefined);
    setSampleFolder('');
    setSampleTicks(new Set<string>());
    setUploads({});
  },
  bar(busy) {
    const info = samplesBar(uploads(), uploadBase(), busy === 'Uploading', uploadAverage());
    return info ? { info, running: busy === 'Uploading' } : undefined;
  },
  lines: () => uploadLines(uploads(), uploadBase()),
  async localChanged() {
    if (!localFolder()) return false;
    const now = await listLocalNow();
    return !!now && listingChanged(localEntries(), now);
  },
};

export const desktopEnv: TransferEnv = {
  library: {
    scan: (device) => invoke<{ path: string; size: number }[]>('lib_scan', { device }),
    read: async (device, path) => new Uint8Array(await invoke<ArrayBuffer>('lib_read', { device, path })),
  },
  patchkitWasm: () => invoke<ArrayBuffer>('patchkit_wasm'),
  deviceName: async (device) => (await deviceStorage(device)).name,
  targets: {
    emulator: true,
    ports: listMidiPorts,
    async open(t: Target) {
      if (t.kind === 'emulator') return new EmuUsbTransport();
      if (t.kind === 'none') throw new Error('No MIDI port.');
      await openMidiPort(t.port);
      return new MidiPortTransport();
    },
    async close(t: Target) {
      if (t.kind === 'midi') await closeMidiPort();
    },
    async preflight(t: Target) {
      if (t.kind === 'emulator' && (await emulatorMode()) === 'paused') throw new Error(PAUSED);
    },
  },
  logRun: (run) => logInput(transferEntry(run)),
  extras: {
    samples,
    onJobEnd() {
      // the Card panel reads the folder now and again after the emulator's quiet period (the folder follows the card about 2 s later)
      bumpCardRefresh();
      window.setTimeout(bumpCardRefresh, 2500);
    },
  },
};

setTransferEnv(desktopEnv);
