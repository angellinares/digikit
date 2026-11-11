// The USB bulk link (packages/dk-session/src/bulk.ts) over WebUSB: Chrome on macOS, Linux and ChromeOS (docs/plans/usb-fast.md section 7). The `usb` object is
// passed in, so the tests give it a fake. Opening a link uses `navigator.usb.getDevices()` (the devices the user already allowed) and never asks:
// `requestAccess()` shows the browser's device chooser and needs a user gesture (a click), so the page calls it from an explicit action.
// Written without TS-only runtime syntax so node can import it.
import { BULK_CLASS, BULK_VID } from '@digi/dk-session/gen_bulk';
import { BulkFrames, sendWithZlp, type BulkLink } from '@digi/dk-session/bulk';
import type { BulkCandidate, BulkOpener } from '@digi/dk-session';

/** The parts of the WebUSB API this file uses. */
export interface UsbEndpointLike {
  endpointNumber: number;
  direction: 'in' | 'out';
  type: string;
}
export interface UsbAlternateLike {
  alternateSetting: number;
  interfaceClass: number;
  endpoints: UsbEndpointLike[];
}
export interface UsbInterfaceLike {
  interfaceNumber: number;
  alternates: UsbAlternateLike[];
}
export interface UsbDeviceLike {
  vendorId: number;
  productName?: string;
  configuration?: { interfaces: UsbInterfaceLike[] } | null;
  configurations?: { configurationValue: number }[];
  opened?: boolean;
  open(): Promise<void>;
  close(): Promise<void>;
  selectConfiguration(value: number): Promise<void>;
  claimInterface(n: number): Promise<void>;
  releaseInterface(n: number): Promise<void>;
  selectAlternateInterface?(n: number, alt: number): Promise<void>;
  clearHalt?(direction: 'in' | 'out', endpoint: number): Promise<void>;
  transferOut(endpoint: number, data: Uint8Array): Promise<{ status: string; bytesWritten?: number }>;
  transferIn(endpoint: number, length: number): Promise<{ status: string; data?: DataView }>;
}
export interface UsbLike {
  getDevices(): Promise<UsbDeviceLike[]>;
  requestDevice(o: { filters: { vendorId?: number; classCode?: number }[] }): Promise<UsbDeviceLike>;
}

export const NO_WEBUSB = 'This browser has no WebUSB. Use Chrome or Edge, or the desktop app.';
/** One IN transfer: replies are small; a multiple of every bulk packet size. */
const IN_SIZE = 4096;

/** The browser's `navigator.usb`, or undefined (Firefox, Safari, a page that is not secure). */
export function webUsb(): UsbLike | undefined {
  return (globalThis.navigator as unknown as { usb?: UsbLike } | undefined)?.usb;
}
export const webUsbAvailable = (): boolean => webUsb() !== undefined;

/** The vendor interface of the device with a bulk OUT and a bulk IN endpoint. */
export function findBulk(
  d: UsbDeviceLike,
): { interfaceNumber: number; alt: number; out: number; inp: number } | undefined {
  for (const i of d.configuration?.interfaces ?? [])
    for (const a of i.alternates) {
      if (a.interfaceClass !== BULK_CLASS) continue;
      const out = a.endpoints.find((e) => e.type === 'bulk' && e.direction === 'out');
      const inp = a.endpoints.find((e) => e.type === 'bulk' && e.direction === 'in');
      if (out && inp)
        return {
          interfaceNumber: i.interfaceNumber,
          alt: a.alternateSetting,
          out: out.endpointNumber,
          inp: inp.endpointNumber,
        };
    }
  return undefined;
}

export class WebUsbBulkLink implements BulkLink {
  private dev: UsbDeviceLike;
  private ep: { interfaceNumber: number; out: number; inp: number };
  private frames = new BulkFrames();
  private ready: Uint8Array[] = [];
  private waiter: (() => void) | null = null;
  private closed = false;
  private failure: Error | null = null;

  private constructor(dev: UsbDeviceLike, ep: { interfaceNumber: number; out: number; inp: number }) {
    this.dev = dev;
    this.ep = ep;
  }

  /**
   * Ask the user for a device (the browser's chooser, filtered by vendor id and the vendor interface class). Needs a user gesture. The device is not
   * opened here: the permission is what `getDevices()` returns afterwards.
   */
  static async requestAccess(usb: UsbLike | undefined = webUsb()): Promise<void> {
    if (!usb) throw new Error(NO_WEBUSB);
    await usb.requestDevice({ filters: [{ vendorId: BULK_VID, classCode: BULK_CLASS }] });
  }

  /** Open a device the page may use (`navigator.usb.getDevices()`: no chooser). Claiming a vendor interface of a composite device whose other interfaces belong to the OS MIDI driver is **[O]** on macOS (plan, open question 4). */
  static async open(dev: UsbDeviceLike): Promise<WebUsbBulkLink> {
    if (!dev.opened) await dev.open();
    if (!dev.configuration) await dev.selectConfiguration(dev.configurations?.[0]?.configurationValue ?? 1);
    const found = findBulk(dev);
    if (!found) {
      await dev.close().catch(() => {});
      throw new Error('This device has no bulk interface (is the usb-fast module loaded?)');
    }
    try {
      await dev.claimInterface(found.interfaceNumber);
      if (found.alt) await dev.selectAlternateInterface?.(found.interfaceNumber, found.alt);
    } catch (e) {
      await dev.close().catch(() => {});
      throw new Error(`Could not claim the bulk interface: ${e instanceof Error ? e.message : String(e)}`);
    }
    const link = new WebUsbBulkLink(dev, found);
    void link.pump();
    return link;
  }

  /** The IN endpoint is read all the time: a reply never waits for a recv, and a frame may arrive in several packets. */
  private async pump(): Promise<void> {
    while (!this.closed) {
      try {
        const r = await this.dev.transferIn(this.ep.inp, IN_SIZE);
        if (r.status === 'stall') {
          await this.dev.clearHalt?.('in', this.ep.inp);
          continue;
        }
        if (r.status !== 'ok' || !r.data) throw new Error(`bulk IN: ${r.status}`);
        // the device sends 64-byte IN packets, each a short packet, and no zero-length packets: `feed` returns a frame when it is complete and the loop reads on
        this.ready.push(...this.frames.feed(new Uint8Array(r.data.buffer, r.data.byteOffset, r.data.byteLength)));
      } catch (e) {
        if (!this.closed) this.failure = e instanceof Error ? e : new Error(String(e));
        this.closed = true;
      }
      this.waiter?.();
    }
  }

  async send(frame: Uint8Array): Promise<void> {
    if (this.closed) throw this.failure ?? new Error('The bulk link is closed.');
    // a frame that is a multiple of 512 bytes is followed by a zero-length transfer (the device completes its RX buffer on a short packet)
    await sendWithZlp(async (data) => {
      const r = await this.dev.transferOut(this.ep.out, data);
      if (r.status === 'stall') await this.dev.clearHalt?.('out', this.ep.out);
      if (r.status !== 'ok') throw new Error(`bulk OUT: ${r.status}`);
    }, frame);
  }

  async recv(timeoutMs: number, signal?: AbortSignal): Promise<Uint8Array | null> {
    const have = () => this.ready.shift();
    for (;;) {
      const f = have();
      if (f) return f;
      if (this.closed) throw this.failure ?? new Error('The bulk link is closed.');
      if (signal?.aborted) return null;
      const timedOut = await new Promise<boolean>((resolve) => {
        const done = (t: boolean) => {
          clearTimeout(timer);
          signal?.removeEventListener('abort', onAbort);
          this.waiter = null;
          resolve(t);
        };
        const onAbort = () => done(true);
        const timer = setTimeout(() => done(true), timeoutMs);
        signal?.addEventListener('abort', onAbort, { once: true });
        this.waiter = () => done(false);
      });
      if (timedOut && !this.ready.length) return null;
    }
  }

  async close(): Promise<void> {
    this.closed = true;
    this.waiter?.();
    try {
      await this.dev.releaseInterface(this.ep.interfaceNumber);
    } catch {
      // the device may be gone already
    }
    await this.dev.close().catch(() => {});
  }
}

/**
 * The devices the page may use that offer the bulk interface, as the `BulkOpener` of the identity handshake (bulk-select.ts). The ids are positions in the
 * list of the last `candidates()` call.
 */
export function webUsbOpener(usb: UsbLike | undefined = webUsb()): BulkOpener {
  let seen: UsbDeviceLike[] = [];
  return {
    async candidates(): Promise<BulkCandidate[]> {
      seen = usb
        ? (await usb.getDevices()).filter((d) => d.vendorId === BULK_VID && (findBulk(d) || !d.configuration))
        : [];
      return seen.map((d, i) => ({ id: String(i), name: d.productName }));
    },
    async open(id: string): Promise<BulkLink> {
      const dev = seen[Number(id)];
      if (!dev) throw new Error(`no USB device ${id}`);
      return WebUsbBulkLink.open(dev);
    },
  };
}
