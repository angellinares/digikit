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
