import { BulkFrames, type BulkLink } from './bulk.ts';

/**
 * A bulk link over a byte stream, such as the stdin and stdout of the `dkbulk open` bridge (packages/dkbulk). The host supplies the two ends as plain
 * functions (this file imports nothing from `node:`): `write` sends bytes, `subscribe` registers the one handler that gets every incoming chunk. Frames
 * are cut from the stream by their own length field. A corrupt stream fails the link: every later `recv` and `send` rejects with the reason.
 * (The bridge does the zero-length packets: a frame goes in whole.)
 */
export class ProcessBulkLink implements BulkLink {
  private write: (bytes: Uint8Array) => void;
  private end: () => void;
  private frames = new BulkFrames();
  private ready: Uint8Array[] = [];
  private waiter: (() => void) | null = null;
  private failure: Error | null = null;
  constructor(
    write: (bytes: Uint8Array) => void,
    subscribe: (onData: (chunk: Uint8Array) => void) => void,
    end: () => void = () => {},
  ) {
    this.write = write;
    this.end = end;
    subscribe((chunk) => {
      try {
        this.ready.push(...this.frames.feed(chunk));
      } catch (e) {
        this.failure = e instanceof Error ? e : new Error(String(e));
        this.end();
      }
      this.waiter?.();
    });
  }

  async send(frame: Uint8Array): Promise<void> {
    if (this.failure) throw this.failure;
    this.write(frame);
  }

  recv(timeoutMs: number, signal?: AbortSignal): Promise<Uint8Array | null> {
    const queued = this.ready.shift();
    if (queued) return Promise.resolve(queued);
    if (this.failure) return Promise.reject(this.failure);
    if (signal?.aborted) return Promise.resolve(null);
    return new Promise((resolve, reject) => {
      const done = (f: Uint8Array | null, err?: Error) => {
        clearTimeout(timer);
        signal?.removeEventListener('abort', onAbort);
        this.waiter = null;
        if (err) reject(err);
        else resolve(f);
      };
      const onAbort = () => done(null);
      const timer = setTimeout(() => done(null), timeoutMs);
      signal?.addEventListener('abort', onAbort, { once: true });
      this.waiter = () => {
        const f = this.ready.shift();
        if (f) done(f);
        else if (this.failure) done(null, this.failure);
      };
    });
  }

  close(): void {
    this.end();
  }
}
