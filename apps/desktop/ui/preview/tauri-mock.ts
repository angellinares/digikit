// A stand-in for `@tauri-apps/api/core` in the preview: just enough for the target selector and a real `DkSession` over the emulator's
// USB capture (a device that never answers: the "no reply" state). Everything else the pages ask of Tauri is answered by the two backend mocks.
import { cardCall, pickFolder } from './card-mock';
import * as firmware from './firmware-mock';
import { sim } from './sim';

export async function invoke<T>(cmd: string, args?: any, _options?: { headers?: Record<string, string> }): Promise<T> {
  switch (cmd) {
    case 'local_pick_folder':
      return (await pickFolder()) as T;
    case 'card_open_folder':
      return undefined as T;
    case 'midi_ports':
      return (firmware.page ? firmware.ports() : ['Elektron Digitakt II', 'IAC Driver Bus 1']) as T;
    case 'midi_open':
      return undefined as T;
    case 'midi_close':
      firmware.midi.close();
      return undefined as T;
    case 'midi_cancel':
      firmware.midi.cancel();
      return undefined as T;
    case 'midi_send':
      firmware.midi.send(args as Uint8Array);
      return undefined as T;
    case 'midi_recv':
      return (await firmware.midi.recv(Number(args?.timeoutMs))) as T;
    case 'patchkit_wasm':
      return (await firmware.patchkitWasm()) as T;
    case 'firmware_prepare':
      return (await firmware.prepare(args as Uint8Array, _options?.headers?.['x-restore'] === '1', _options?.headers?.['x-set'] ?? '')) as T;
    case 'firmware_save':
      return firmware.save(
        decodeURIComponent(_options?.headers?.['x-name'] ?? 'firmware.syx'),
        args as Uint8Array,
      ) as T;
    case 'log_save': {
      // the Save button of the Log drawer: keep the text where a script can read it, answer with a path
      const name = decodeURIComponent(_options?.headers?.['x-name'] ?? 'log.log');
      (window as unknown as { __saved: { name: string; text: string }[] }).__saved ??= [];
      (window as unknown as { __saved: { name: string; text: string }[] }).__saved.push({
        name,
        text: new TextDecoder().decode(args as Uint8Array),
      });
      return `/Users/em/Downloads/${name}` as T;
    }
    case 'emu_call': {
      const method = String(args?.method);
      if (method === 'status') return { mode: sim.paused ? 'paused' : 'running' } as T;
      if (method.startsWith('card.'))
        return (await cardCall(method, (args?.params as Record<string, unknown>) ?? {})) as T;
      return {} as T; // midi.usb.in / midi.usb.out: nothing is ever answered
    }
    default:
      throw new Error(`preview: no mock for the Tauri command ${cmd}`);
  }
}
