// Which USB device is the one on the MIDI port (docs/plans/usb-fast.md section 5). Several Elektron devices can be attached and their USB serial strings are
// the same, so the host proves it with a nonce: it sends 8 random bytes to the selected MIDI target (SET_NONCE, a SysEx service command of the usb-fast
// module), opens each device that offers the bulk interface, asks INFO and uses only the one that returns the nonce. Used by the desktop app and by
// tools/devtools/wb.mjs (the same code). Written without TS-only runtime syntax so node can import it.
import { BulkSession, type BulkLink, type BulkOptions } from './bulk.ts';
import { BULK_NONCE_LEN } from './gen_bulk.ts';

/** A device that offers the bulk interface (the desktop: `bus:address`; the browser: an index of the devices the page may use). */
export interface BulkCandidate {
  id: string;
  name?: string;
}
/** What the app can do. */
export interface BulkOpener {
  /** The devices that offer the bulk interface now (cheap, no claim). Empty: no bulk link, stay on SysEx. */
  candidates(): Promise<BulkCandidate[]>;
  open(id: string): Promise<BulkLink>;
}
/** The SysEx side: `DkSession.usbFastNonce`. */
export interface NonceSender {
  usbFastNonce(nonce: Uint8Array): Promise<void>;
}

export interface BulkChoice {
  session: BulkSession;
  link: BulkLink;
  id: string;
}

const same = (a: Uint8Array, b: Uint8Array) => a.length === b.length && a.every((x, i) => x === b[i]);
const why = (e: unknown) => (e instanceof Error ? e.message : String(e));

/**
 * The bulk session for the device on the MIDI target, or undefined (with the reason in `note`): no candidate (not logged: usb-fast is not loaded), the
 * nonce could not be set, no candidate returned it. Every candidate that is not chosen is closed.
 */
export async function chooseBulk(
  sysex: NonceSender,
  opener: BulkOpener,
  note: (line: string) => void = () => {},
  options: BulkOptions = {},
): Promise<BulkChoice | undefined> {
  let candidates: BulkCandidate[];
  try {
    candidates = await opener.candidates();
  } catch (e) {
    note(`USB bulk link not used: listing the devices failed: ${why(e)}`);
    return undefined;
  }
  if (candidates.length === 0) return undefined;
  const nonce = new Uint8Array(BULK_NONCE_LEN);
  globalThis.crypto.getRandomValues(nonce);
  try {
    await sysex.usbFastNonce(nonce);
  } catch (e) {
    note(
      `USB bulk link not used: the device on the MIDI port did not take the nonce (usb-fast not loaded?): ${why(e)}`,
    );
    return undefined;
  }
  for (const c of candidates) {
    let link: BulkLink | undefined;
    try {
      link = await opener.open(c.id);
      const session = new BulkSession(link, options);
      if (same((await session.info()).nonce, nonce)) return { session, link, id: c.id };
      note(`USB device ${c.name ?? c.id}: not the one on the MIDI port (other nonce)`);
    } catch (e) {
      note(`USB device ${c.name ?? c.id}: ${why(e)}`);
    }
    await Promise.resolve(link?.close()).catch(() => {});
  }
  note(`USB bulk link not used: none of ${candidates.length} candidate(s) returned this device's nonce`);
  return undefined;
}

// ---- take-over (docs/plans/usb-fast-takeover.md) ----------------------------------------------------------------------------------------------
// In USB AUDIO/MIDI mode the device has no bulk interface. A transfer batch (one user action, for example 100 dropped files) takes the USB port over: TAKEOVER
// over SysEx, the device re-enumerates in USB MIDI mode (its MIDI port may vanish and return, under another name), the nonce handshake (chooseBulk) finds the
// bulk interface, the batch runs, and `lingerMs` after the last batch RELEASE gives the mode back. A second batch within the linger reuses the link.

/** The SysEx side of the take-over: `DkSession.usbFastNonce`, `usbFastTakeOver` and `usbFastRelease`. */
export interface TakeOverSysex extends NonceSender {
  /** True when the device is not in USB MIDI mode: the app takes over only then (no flag, no question). */
  usbFastWrongMode(): Promise<boolean>;
  usbFastTakeOver(): Promise<void>;
  usbFastRelease(): Promise<void>;
}
/** The app's view of the device across the re-enumeration. */
export interface TakeOverDevice {
  /** The SysEx session that reaches the device now. After a switch it may be another one: the MIDI port can vanish and return, possibly under another name. */
  sysex(): TakeOverSysex;
  /**
   * Resolves when the device is back after the switch and `sysex()` reaches it. The app finds it again by its USB identity (the id of its existing session,
   * not the MIDI port name); it resolves at once when the device never left (it was in USB MIDI mode already). Rejects at the timeout.
   */
  reenumerated(timeoutMs: number): Promise<void>;
}
export interface TakeOverOptions {
  /** How long after the last batch the take-over is kept (default 10 s). */
  lingerMs?: number;
  /** The most the switch, the re-enumeration and the handshake may take (default 20 s). */
  startTimeoutMs?: number;
  /** The wait between handshake tries while the bulk interface is not there yet (default 250 ms). */
  pollMs?: number;
  /** How long an idle held link may take to answer the check at the start of a batch (default 2 s). A silent link is dropped and the take-over starts again. */
  aliveMs?: number;
  note?: (line: string) => void;
  bulk?: BulkOptions;
}

const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

export class BulkTakeOver {
  private device: TakeOverDevice;
  private opener: BulkOpener;
  private lingerMs: number;
  private startTimeoutMs: number;
  private pollMs: number;
  private aliveMs: number;
  private note: (line: string) => void;
  private bulk: BulkOptions;
  private held: BulkChoice | undefined;
  private starting: Promise<BulkChoice | undefined> | undefined;
  private releasing: Promise<void> | undefined;
  private batches = 0;
  private timer: ReturnType<typeof setTimeout> | undefined;

  constructor(device: TakeOverDevice, opener: BulkOpener, options: TakeOverOptions = {}) {
    this.device = device;
    this.opener = opener;
    this.lingerMs = options.lingerMs ?? 10_000;
    this.startTimeoutMs = options.startTimeoutMs ?? 20_000;
    this.pollMs = options.pollMs ?? 250;
    this.aliveMs = options.aliveMs ?? 2000;
    this.note = options.note ?? (() => {});
    this.bulk = options.bulk ?? {};
  }

  /** True while the device is taken over (a batch runs or the linger runs). */
  get active(): boolean {
    return this.held !== undefined;
  }

  /**
   * Start a batch: the bulk link of the taken-over device, or undefined (the reason went to `note`: the device refused, did not come back, or has no bulk
   * interface; the device is released then). Undefined: the batch did not start, do not call `end`. Several batches may run at once.
   */
  async begin(): Promise<BulkChoice | undefined> {
    this.cancelLinger();
    this.batches++;
    try {
      if (this.releasing) await this.releasing;
      if (this.held && (await this.alive(this.held))) return this.held;
      if (!this.starting) this.starting = this.start().finally(() => (this.starting = undefined));
      const c = await this.starting;
      if (!c) this.batches--;
      return c;
    } catch (e) {
      this.batches--;
      throw e;
    }
  }

  /**
   * Is the held link still the device? The device gives the mode back by itself (30 s idle, bus suspend, unplug), and the link then fails. A link that reports
   * closed is dead. An idle link (no other batch runs on it) is asked INFO; a silent one is dead. A dead link is dropped and closed: the caller starts a new take-over.
   * A link in use by another batch is not asked (a session runs one request at a time): its own batch sees the failure.
   */
  private async alive(h: BulkChoice): Promise<boolean> {
    let ok = !h.link.closed && !h.session.failed;
    if (ok && this.batches <= 1) {
      let timer: ReturnType<typeof setTimeout> | undefined;
      try {
        await Promise.race([
          h.session.info(),
          new Promise<never>((_, rej) => (timer = setTimeout(() => rej(new Error('no answer')), this.aliveMs))),
        ]);
      } catch (e) {
        this.note(`USB bulk link lost (the device gave the mode back?): ${why(e)}; taking over again`);
        ok = false;
      } finally {
        clearTimeout(timer);
      }
    } else if (!ok) {
      this.note('USB bulk link lost (closed); taking over again');
    }
    if (!ok && this.held === h) {
      this.held = undefined;
      await Promise.resolve(h.link.close()).catch(() => {});
    }
    return ok;
  }

  /** End a batch that `begin` started. The last one arms the linger timer. */
  end(): void {
    if (this.batches > 0) this.batches--;
    if (this.batches === 0 && this.held && !this.timer) {
      this.timer = setTimeout(() => {
        this.timer = undefined;
        void this.release();
      }, this.lingerMs);
    }
  }

  /** Release at once (the app closes or loses the device). */
  async close(): Promise<void> {
    this.cancelLinger();
    this.batches = 0;
    if (this.starting) await this.starting.catch(() => undefined);
    await this.release();
  }

  private cancelLinger(): void {
    if (this.timer) clearTimeout(this.timer);
    this.timer = undefined;
  }

  private release(): Promise<void> {
    const h = this.held;
    if (!h) return this.releasing ?? Promise.resolve();
    this.held = undefined;
    this.releasing = (async () => {
      await Promise.resolve(h.link.close()).catch(() => {});
      await this.sendRelease();
    })().finally(() => (this.releasing = undefined));
    return this.releasing;
  }

  private async sendRelease(): Promise<void> {
    try {
      await this.device.sysex().usbFastRelease();
    } catch (e) {
      this.note(`USB release failed (the device gives the mode back by itself after 30 s without traffic): ${why(e)}`);
    }
  }

  private async start(): Promise<BulkChoice | undefined> {
    try {
      await this.device.sysex().usbFastTakeOver();
    } catch (e) {
      this.note(
        `USB bulk link not used: the take-over was refused (usb-fast not loaded, or Overbridge mode): ${why(e)}`,
      );
      return undefined;
    }
    const deadline = Date.now() + this.startTimeoutMs;
    try {
      await this.device.reenumerated(this.startTimeoutMs);
    } catch (e) {
      this.note(`USB bulk link not used: the device did not come back after the switch: ${why(e)}`);
      await this.sendRelease();
      return undefined;
    }
    let lines: string[] = [];
    for (;;) {
      lines = [];
      const c = await chooseBulk(this.device.sysex(), this.opener, (l) => lines.push(l), this.bulk);
      if (c) {
        this.held = c;
        return c;
      }
      if (Date.now() >= deadline) break;
      await sleep(this.pollMs);
    }
    for (const l of lines) this.note(l);
    this.note(`USB bulk link not used: no bulk interface within ${this.startTimeoutMs} ms of the take-over`);
    await this.sendRelease();
    return undefined;
  }
}
