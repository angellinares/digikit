// The user's stock firmware, kept in this browser or webview (a BlobStore) so that the Transfer pane can bind service files (`.dks`) to it
// without asking for the file again. The Firmware page remembers the stock image it prepares; nothing leaves the machine and nothing is committed.
// Keys: `device/<id>` holds the hash of the newest image of a device, `image/<hash>` its bytes.
import type { BlobStore } from '../blob-store';

const enc = new TextEncoder();
const dec = new TextDecoder();

export interface StockStore {
  /** Keep the stock image of `device` (`dt2`, `dn2`) with its SHA-256 (`PreparedFlash.shaStock`); it replaces the older image of that device. */
  remember(device: string, hash: string, bytes: Uint8Array): Promise<void>;
  /** The hash of the kept image of `device`, or undefined when there is none. */
  hashOf(device: string): Promise<string | undefined>;
  /** The kept bytes with this hash. */
  bytesOf(hash: string): Promise<Uint8Array | undefined>;
}

export function createStockStore(blobs: BlobStore): StockStore {
  return {
    async remember(device, hash, bytes) {
      const old = await blobs.get(`device/${device}`);
      await blobs.put(`image/${hash}`, bytes);
      await blobs.put(`device/${device}`, enc.encode(hash));
      if (old && dec.decode(old) !== hash) await blobs.delete(`image/${dec.decode(old)}`);
    },
    async hashOf(device) {
      const h = await blobs.get(`device/${device}`);
      return h ? dec.decode(h) : undefined;
    },
    bytesOf: (hash) => blobs.get(`image/${hash}`),
  };
}
