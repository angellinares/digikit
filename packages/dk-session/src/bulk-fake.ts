// A device on the other end of a `BulkLink`, with the device rules of the plan (docs/plans/usb-fast.md sections 3.1 and 5): one write session, DATA at
// the next offset only (a multiple of 512 except the last piece), E_BUSY while the store is in use, the END hash check, ABORT, VERIFY, REPLACE (the old
// file stays until END commits) and the nonce of the identity handshake. It models the USB rules too: an OUT transfer that is a multiple of 512 bytes is
// not delivered until a short transfer ends it (`out`), and `inTransfers` gives what the IN endpoint delivers (a frame that is a multiple of 512 bytes is
// followed by a zero-length transfer). For host tests of the session, the samples backend, the desktop link and the WebUSB link; faults by field.
import { fileHash } from './dkupload-core.ts';
import { bulkFrameLength, bulkReply, parseBulkRequest, sendWithZlp, type BulkLink } from './bulk.ts';
import {
  BULK_ALIGN,
  BULK_CMD,
  BULK_DATA_MAX,
  BULK_FLAG,
  BULK_NONCE_LEN,
  BULK_PACKET,
  BULK_STATUS,
} from './gen_bulk.ts';

/** The file hash the store keeps: the content hash with bit 0 set (`hash|1`), as 4 bytes. */
export const hash4 = (d: Uint8Array) => new DataView(fileHash(d).buffer, 0, 4).getUint32(0);

export class FakeBulkDevice implements BulkLink {
  /** The files: path -> bytes. */
  files = new Map<string, Uint8Array>();
  /** Answer E_BUSY to the next n requests of a command (nothing is applied). */
  busy = new Map<number, number>();
  /** After each applied DATA, answer E_BUSY to the next `[0]` DATA requests, at most `[1]` times. */
  busyBursts: [number, number] = [0, 0];
  private burstLeft = 0;
  corruptEndHash = false;
  /** Requests of these commands are dropped: no effect, no reply. */
  mute = new Set<number>();
  /** Hold the replies back until `release()`. */
  hold = false;
  /** Bytes added to the INFO reply body. */
  infoPad = 0;
  nonce = new Uint8Array(BULK_NONCE_LEN);
  /** The device keeps ZLT on EP2 IN (the real one does). */
  zlt = true;
  /** The requests seen: [cmd, flags, offset-or-0]. */
  log: [number, number, number][] = [];
  /** Every DATA offset the device applied. */
  applied: number[] = [];
  maxInflight = 0;
  closed = false;
  badFrames = 0;
  private inflight = 0;
  private last = new Uint8Array();
  private session: { path: string; size: number; hash: number; data: Uint8Array[]; got: number } | null = null;
  private partial: Uint8Array[] = [];
  private held: Uint8Array[] = [];
  private replies: Uint8Array[] = [];
  private waiter: (() => void) | null = null;

  get sessionOpen() {
    return this.session !== null;
  }
  /** Bytes of OUT transfers that no short transfer has completed yet. */
  get pendingOut() {
    return this.partial.reduce((n, p) => n + p.length, 0);
  }
  /** SET_NONCE (the SysEx service command of the usb-fast module). */
  setNonce(n: Uint8Array) {
    if (n.length !== BULK_NONCE_LEN) throw new Error('nonce of 8 bytes');
    this.nonce = n.slice();
  }
  release() {
    this.hold = false;
    this.replies.push(...this.held.splice(0));
    this.waiter?.();
  }

  /** One OUT transfer. A transfer that is a multiple of 512 bytes ends no frame; a short or zero-length one does. */
  out(data: Uint8Array): void {
    this.partial.push(data.slice());
    if (data.length > 0 && data.length % BULK_PACKET === 0) return;
    const n = this.pendingOut;
    if (n === 0) return;
    const frame = new Uint8Array(n);
    let at = 0;
    for (const p of this.partial.splice(0)) {
      frame.set(p, at);
      at += p.length;
    }
    this.deliver(frame);
  }

  /** The BulkLink send: the ZLP rule over `out`. */
  async send(frame: Uint8Array): Promise<void> {
    await sendWithZlp(async (d) => this.out(d), frame);
  }

  /** What the IN endpoint delivers for a reply: pieces of at most 512 bytes, then a zero-length transfer when the frame is a multiple of 512 and ZLT is on. */
  inTransfers(frame: Uint8Array): Uint8Array[] {
    const out: Uint8Array[] = [];
    for (let i = 0; i < frame.length; i += BULK_PACKET) out.push(frame.slice(i, i + BULK_PACKET));
    if (this.zlt && frame.length > 0 && frame.length % BULK_PACKET === 0) out.push(new Uint8Array());
    return out;
  }

  async recv(timeoutMs: number, signal?: AbortSignal): Promise<Uint8Array | null> {
    if (!this.replies.length && timeoutMs > 0 && !signal?.aborted)
      await new Promise<void>((resolve) => {
        const done = () => {
          clearTimeout(t);
          signal?.removeEventListener('abort', done);
          this.waiter = null;
          resolve();
        };
        const t = setTimeout(done, Math.min(timeoutMs, 50)); // a silent device: the tests need no long wait
        signal?.addEventListener('abort', done, { once: true });
        this.waiter = done;
      });
    const r = this.replies.shift();
    if (r) this.inflight = Math.max(0, this.inflight - 1);
    return r ?? null;
  }

  close() {
    this.closed = true;
  }

  private deliver(frame: Uint8Array) {
    let req;
    try {
      if (bulkFrameLength(frame) !== frame.length) throw new Error('length');
      req = parseBulkRequest(frame);
    } catch {
      this.badFrames++;
      return;
    }
    const { cmd, seq, flags, body } = req;
    const off =
      cmd === BULK_CMD.DATA && body.length >= 4 ? new DataView(body.buffer, body.byteOffset, 4).getUint32(0) : 0;
    this.log.push([cmd, flags, off]);
    this.inflight++;
    this.maxInflight = Math.max(this.maxInflight, this.inflight);
    if (this.mute.has(cmd)) return;
    const r = this.handle(cmd, seq, flags, body);
    if (this.hold) this.held.push(r);
    else {
      this.replies.push(r);
      this.waiter?.();
    }
  }

  private handle(cmd: number, seq: number, flags: number, body: Uint8Array): Uint8Array {
    const err = (s: number) => bulkReply(cmd, seq, s);
    const left = this.busy.get(cmd) ?? 0;
    if (left > 0) {
      this.busy.set(cmd, left - 1);
      return err(BULK_STATUS.L_E_BUSY);
    }
    if (cmd === BULK_CMD.DATA && this.burstLeft > 0) {
      this.burstLeft--;
      return err(BULK_STATUS.L_E_BUSY);
    }
    const v = new DataView(body.buffer, body.byteOffset, body.length);
    switch (cmd) {
      case BULK_CMD.INFO: {
        const b = new Uint8Array(12 + BULK_NONCE_LEN + this.infoPad);
        b[0] = 1;
        new DataView(b.buffer).setUint32(4, BULK_DATA_MAX);
        new DataView(b.buffer).setUint32(8, BULK_ALIGN);
        b.set(this.nonce, 12);
        return bulkReply(cmd, seq, 0, b);
      }
      case BULK_CMD.BEGIN: {
        if (this.session) return err(BULK_STATUS.L_E_BUSY);
        const nul = body.indexOf(0, 4);
        if (nul < 0 || body.length !== nul + 9) return err(BULK_STATUS.E_MSG);
        const path = new TextDecoder().decode(body.subarray(4, nul));
        if (this.files.has(path) && !(flags & BULK_FLAG.REPLACE)) return err(BULK_STATUS.L_E_EXISTS);
        this.session = { path, size: v.getUint32(nul + 1), hash: v.getUint32(nul + 5), data: [], got: 0 };
        return bulkReply(cmd, seq, 0);
      }
      case BULK_CMD.DATA: {
        const s = this.session;
        if (!s) return err(BULK_STATUS.E_STATE);
        if (body.length < 4) return err(BULK_STATUS.E_MSG);
        const offset = v.getUint32(0);
        const bytes = body.subarray(4);
        if (offset !== s.got) return err(BULK_STATUS.E_OFFSET);
        const last = s.got + bytes.length === s.size;
        if (
          !bytes.length ||
          s.got + bytes.length > s.size ||
          bytes.length > BULK_DATA_MAX ||
          (bytes.length % BULK_ALIGN !== 0 && !last)
        )
          return err(BULK_STATUS.E_LENGTH);
        s.data.push(bytes.slice());
        s.got += bytes.length;
        this.applied.push(offset);
        if (this.busyBursts[1] > 0) {
          this.busyBursts[1]--;
          this.burstLeft = this.busyBursts[0];
        }
        return bulkReply(cmd, seq, 0);
      }
      case BULK_CMD.END: {
        const s = this.session;
        if (!s) return err(BULK_STATUS.E_STATE);
        this.session = null;
        if (s.got !== s.size) return err(BULK_STATUS.E_LENGTH);
        const data = new Uint8Array(s.size);
        let at = 0;
        for (const p of s.data) {
          data.set(p, at);
          at += p.length;
        }
        const h = hash4(data);
        if (s.hash !== 0 && s.hash !== h) return err(BULK_STATUS.E_HASH);
        this.last = data;
        this.files.set(s.path, data); // the old file is replaced here, at END, never before
        const b = new Uint8Array(4);
        new DataView(b.buffer).setUint32(0, this.corruptEndHash ? (h ^ 2) >>> 0 : h);
        return bulkReply(cmd, seq, 0, b);
      }
      case BULK_CMD.ABORT:
        this.session = null;
        return bulkReply(cmd, seq, 0);
      case BULK_CMD.VERIFY: {
        const off = v.getUint32(0);
        const len = v.getUint32(4);
        if (off + len > this.last.length) return err(BULK_STATUS.E_READ);
        const b = new Uint8Array(4);
        new DataView(b.buffer).setUint32(0, hash4(this.last.subarray(off, off + len)));
        return bulkReply(cmd, seq, 0, b);
      }
      default:
        return err(BULK_STATUS.E_PROTO);
    }
  }
}
