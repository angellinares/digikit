import { splitMessages, type DkTransport } from './dkupload-core.ts';

/**
 * A SysEx link over a byte stream, such as the stdin and stdout of the `dkmidi` bridge. The host supplies the two ends as plain functions
 * (this file imports nothing from `node:`): `write` sends bytes, `subscribe` registers the one handler that gets every incoming chunk.
 * Chunks may split or join messages; `splitMessages` frames them.
 */
export class ProcessTransport implements DkTransport {
  constructor(write: (bytes: Uint8Array) => void, subscribe: (onData: (chunk: Uint8Array) => void) => void) {
    this.write = write;
    subscribe((chunk) => this.feed(chunk));
  }

  private readonly write: (bytes: Uint8Array) => void;
  private rx: Uint8Array = new Uint8Array();
  private ready: Uint8Array[] = [];
  /** The recv that waits, if any (the session makes one request at a time). */
  private waiter: ((msg: Uint8Array) => void) | null = null;

  send(msg: Uint8Array): void {
    this.write(msg);
  }

  private feed(chunk: Uint8Array): void {
    const joined = new Uint8Array(this.rx.length + chunk.length);
    joined.set(this.rx);
    joined.set(chunk, this.rx.length);
    const { messages, rest } = splitMessages(joined);
    this.rx = rest;
    this.ready.push(...messages);
    const wake = this.waiter;
    const next = wake ? this.ready.shift() : undefined;
    if (wake && next) wake(next);
  }

  /** The next complete message, or null after `timeoutMs` or when `signal` aborts. */
  recv(timeoutMs: number, signal?: AbortSignal): Promise<Uint8Array | null> {
    const queued = this.ready.shift();
    if (queued) return Promise.resolve(queued);
    if (signal?.aborted) return Promise.resolve(null);
    return new Promise((resolve) => {
      const done = (msg: Uint8Array | null) => {
        clearTimeout(timer);
        signal?.removeEventListener('abort', onAbort);
        this.waiter = null;
        resolve(msg);
      };
      const onAbort = () => done(null);
      const timer = setTimeout(() => done(null), timeoutMs);
      signal?.addEventListener('abort', onAbort, { once: true });
      this.waiter = done;
    });
  }
}
