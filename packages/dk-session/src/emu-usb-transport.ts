import { splitMessages, type DkTransport } from './dkupload-core.ts';

/** The emulator control RPC, supplied by the host shell or a Node client. */
export type EmuRpcCall = <T>(method: string, params?: Record<string, unknown>) => Promise<T>;

/** Messages are handed over in the high-speed model (384 bytes per 16,500 instructions, a microframe), as the kept upload scenarios do. */
const USB_CHUNK = 384;
const USB_PERIOD = 16_500;
const POLL_MS = 10;

const hex = (b: Uint8Array) => Array.from(b, (x) => x.toString(16).padStart(2, '0')).join('');
const bytesOf = (h: string) => Uint8Array.from(h.match(/../g) ?? [], (x) => parseInt(x, 16));
/** The emulator's USB takes any size of MidiRpc write: 16 KiB a request makes a +Drive file about twice as fast as 2 KiB (the firmware's work per request, and the guest idle between requests). */
export const EMU_RPC_CHUNK = 16384;
/** The emulator's runtime takes the next DATA request while it writes the last: two in flight (finding 43). The one place to change it. */
export const EMU_DATA_DEPTH = 2;

/**
 * `stepInstructions`: for a paused daemon (`Emu.spawn`, a boot rig), where time passes only when stepped. An empty poll
 * then steps the emulator that many instructions instead of waiting `pollMs`, as `tools/device/dkupload.py`'s
 * `EmuTransport` does. Leave it unset for a free-running daemon.
 */
export interface EmuUsbOptions {
  pollMs?: number;
  stepInstructions?: number;
}

export class EmuUsbTransport implements DkTransport {
  constructor(privateCall: EmuRpcCall, options: EmuUsbOptions = {}) {
    this.call = privateCall;
    this.pollMs = options.pollMs ?? POLL_MS;
    this.stepInstructions = options.stepInstructions ?? 0;
  }

  private readonly call: EmuRpcCall;
  private readonly pollMs: number;
  private readonly stepInstructions: number;
  readonly rpcChunk = EMU_RPC_CHUNK;
  readonly dataDepth = EMU_DATA_DEPTH;
  private rx: Uint8Array = new Uint8Array();
  private ready: Uint8Array[] = [];

  async send(msg: Uint8Array): Promise<void> {
    await this.call('midi.usb.in', { bytes: hex(msg), chunk: USB_CHUNK, period: USB_PERIOD });
  }

  /** The next complete message the firmware sent, or null after `timeoutMs` or when `signal` aborts. */
  async recv(timeoutMs: number, signal?: AbortSignal): Promise<Uint8Array | null> {
    const end = Date.now() + timeoutMs;
    for (;;) {
      const next = this.ready.shift();
      if (next) return next;
      if (signal?.aborted) return null;
      const { hex: got } = await this.call<{ hex?: string }>('midi.usb.out');
      if (got) {
        const joined = new Uint8Array(this.rx.length + got.length / 2);
        joined.set(this.rx);
        joined.set(bytesOf(got), this.rx.length);
        const { messages, rest } = splitMessages(joined);
        this.rx = rest;
        this.ready.push(...messages);
        continue;
      }
      if (Date.now() >= end || signal?.aborted) return null;
      if (this.stepInstructions > 0) await this.call('step', { instructions: this.stepInstructions });
      else await new Promise((r) => setTimeout(r, this.pollMs));
    }
  }
}
