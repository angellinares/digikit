// The site's side of the Transfer pane (env.ts): the modules the site ships and the .dkm files the user adds (web-library.ts), the Web MIDI ports
// as targets (no emulator), patchkit.wasm from the site, and a console log. `startWebTransfer` asks for MIDI access and picks the first target.
// Every URL goes through BASE_URL (GitHub Pages serves the site under a sub-path).
import type { DkTransport } from '@digi/dk-session';
import { noteConnection } from './connection-state';
import { setTransferEnv, type TransferEnv } from './env';
import { portLost, selectTarget, startOn, target, type Target } from './target';
import { MODULE_FILES } from './module-files.generated';
import { createWebLibrary } from './web-library';
import {
  WebMidiTransport,
  listPorts,
  requestAccess,
  type MidiAccessLike,
  type MidiInputLike,
  type MidiOutputLike,
} from '../modules/webmidi';
import './transfer-site.css';

const BASE = import.meta.env.BASE_URL.replace(/\/?$/, '/');

const fetchOk = async (url: string): Promise<Response> => {
  const r = await fetch(url);
  if (!r.ok) throw new Error(`${url}: ${r.status} ${r.statusText}`.trim());
  return r;
};

const DEVICE_NAMES: Record<string, string> = { dt2: 'Digitakt II', dn2: 'Digitone II' };
export const deviceNameOf = (id: string): string => DEVICE_NAMES[id] ?? id;

/** A port that has a device of ours on it. */
export const isElektronPort = (name: string): boolean => /digitakt|digitone/i.test(name);

/** The names of the ports that have both an input and an output (a device is talked to both ways). */
export function usablePorts(access: MidiAccessLike): string[] {
  const { inputs, outputs } = listPorts(access);
  const ins = new Set(inputs.filter((p) => p.state !== 'disconnected').map((p) => p.name ?? p.id));
  return outputs.filter((p) => p.state !== 'disconnected' && ins.has(p.name ?? p.id)).map((p) => p.name ?? p.id);
}

/** The target to start on: the saved port when it is there, else the first Elektron port, else none. */
export function firstTarget(ports: string[], current: Target): Target {
  if (current.kind === 'midi' && ports.includes(current.port)) return current;
  const first = ports.find(isElektronPort);
  return first ? { kind: 'midi', port: first } : { kind: 'none' };
}

let accessPromise: Promise<MidiAccessLike> | undefined;
/** The Web MIDI access, asked for once; a failure is asked again at the next call. */
function access(): Promise<MidiAccessLike> {
  accessPromise ??= requestAccess().catch((e) => {
    accessPromise = undefined;
    throw e;
  });
  return accessPromise;
}

const open = new Map<string, WebMidiTransport>();

const { library, source } = createWebLibrary({
  bytes: async (path) => new Uint8Array(await (await fetchOk(`${BASE}modules/${path}`)).arrayBuffer()),
  text: async (path) => (await fetchOk(`${BASE}modules/${path}`)).text(),
});

export const webEnv: TransferEnv = {
  library,
  source,
  moduleFiles: (id) => MODULE_FILES[id],
  patchkitWasm: async () => (await fetchOk(`${BASE}patchkit.wasm`)).arrayBuffer(),
  deviceName: async (device) => deviceNameOf(device),
  targets: {
    emulator: false,
    async ports() {
      const a = await access();
      const names = usablePorts(a);
      // a device that was plugged in after the page opened: use it when there is no target yet
      if (target().kind === 'none') {
        const next = firstTarget(names, { kind: 'none' });
        if (next.kind === 'midi') void selectTarget(next).catch(() => {});
      }
      return names;
    },
    async whyNone() {
      // asks again: the user may have allowed MIDI, or plugged the device in, since the page opened
      const names = await access().then(usablePorts);
      return names.length
        ? 'Pick a MIDI port under Target.'
        : 'No MIDI port. Connect the device by USB, set its USB mode to MIDI, then pick it under Target.';
    },
    async open(t: Target): Promise<DkTransport> {
      if (t.kind !== 'midi') throw new Error('No MIDI port.');
      const a = await access();
      const { inputs, outputs } = listPorts(a);
      const input: MidiInputLike | undefined = inputs.find((p) => (p.name ?? p.id) === t.port);
      const output: MidiOutputLike | undefined = outputs.find((p) => (p.name ?? p.id) === t.port);
      if (!input || !output)
        throw new Error(`The MIDI port "${t.port}" is not there. Connect the device and pick the port again.`);
      open.get(t.port)?.close();
      const transport = await WebMidiTransport.open(input, output, a, (e) => {
        noteConnection({ type: 'closed', reason: e.message });
        portLost(t.port);
      });
      open.set(t.port, transport);
      return transport;
    },
    async close(t: Target) {
      if (t.kind !== 'midi') return;
      open.get(t.port)?.close();
      open.delete(t.port);
    },
  },
  logRun(run) {
    const head = `${run.what} on ${run.target}`;
    if (run.error) console.warn(`[transfer] ${head}: ${run.cancelled ? 'cancelled' : run.error}`);
    else console.info(`[transfer] ${head}${run.message ? `: ${run.message}` : ''}`, ...run.lines);
  },
};

setTransferEnv(webEnv);

/**
 * Ask for MIDI access and start on the first device port. Throws the reason (no Web MIDI in this browser, no SysEx permission); the page shows it
 * and the target stays on none. Called again by the page's retry button.
 */
export async function startWebTransfer(): Promise<void> {
  let a: MidiAccessLike;
  try {
    a = await access();
  } catch (e) {
    startOn({ kind: 'none' });
    throw e;
  }
  startOn(firstTarget(usablePorts(a), target()));
}
