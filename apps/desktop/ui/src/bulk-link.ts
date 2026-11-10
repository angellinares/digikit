// The USB bulk link of the desktop app: the Tauri commands of `bulk_port.rs` (nusb) as a `BulkLink` of the TypeScript protocol client
// (packages/dk-session/src/bulk.ts), shaped like midi-transport.ts. The native side moves whole frames and starts no process.
import { invoke } from '@tauri-apps/api/core';
import type { BulkCandidate, BulkLink } from '@digi/dk-session';

/** End the pending `recv` on the native side now (it answers as if its time had run out). */
const cancelBulkRecv = () => invoke<void>('bulk_cancel');

export class TauriBulkLink implements BulkLink {
  /** The devices on the bus that offer the bulk interface (usb-fast loaded): `id` is `bus:address`. Their USB serial strings are the same: the nonce handshake tells which is ours. */
  static candidates = () => invoke<BulkCandidate[]>('bulk_list');
  /** Claim the device `id`; the interface of a link opened before is released first. */
  static async open(id: string): Promise<TauriBulkLink> {
    await invoke<void>('bulk_open', { id });
    return new TauriBulkLink();
  }

  async send(frame: Uint8Array): Promise<void> {
    await invoke('bulk_send', frame);
  }

  /** The next reply frame, or null after `timeoutMs` or when `signal` aborts (the native wait ends too). */
  async recv(timeoutMs: number, signal?: AbortSignal): Promise<Uint8Array | null> {
    if (signal?.aborted) return null;
    const onAbort = () => void cancelBulkRecv().catch(() => {});
    signal?.addEventListener('abort', onAbort, { once: true });
    try {
      const got = await invoke<ArrayBuffer>('bulk_recv', { timeoutMs: Math.max(1, Math.round(timeoutMs)) });
      return got.byteLength ? new Uint8Array(got) : null;
    } finally {
      signal?.removeEventListener('abort', onAbort);
    }
  }

  async close(): Promise<void> {
    await invoke<void>('bulk_close');
  }
}
