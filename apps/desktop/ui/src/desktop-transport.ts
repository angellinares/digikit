// The desktop shell supplies Tauri's emulator RPC to the shared USB SysEx transport.
import { invoke } from '@tauri-apps/api/core';
import { EmuUsbTransport as RpcUsbTransport } from '@digi/dk-session';

export { EMU_RPC_CHUNK } from '@digi/dk-session';

export const emuCall = <T>(method: string, params: Record<string, unknown> = {}) =>
  invoke<T>('emu_call', { method, params, session: null });
const call = emuCall;

export class EmuUsbTransport extends RpcUsbTransport {
  constructor() {
    super(call);
  }
}

/** The controller's run mode: a paused emulator would never answer (nothing steps it). */
export async function emulatorMode(): Promise<string | undefined> {
  return (await call<{ mode?: string }>('status')).mode;
}
