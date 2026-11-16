import net from 'node:net';
import type {
  EmuEvent,
  FirmwareLoadParams,
  FirmwareLoadResult,
  FramebufferParams,
  FramebufferResult,
  GuardReport,
  GuardSetParams,
  InputScheduleEvent,
  LedsResult,
  PcProbeReport,
  PcProbeSetParams,
  RunUntilParams,
  StatusResult,
  WatchdogResult,
} from './types.ts';

export const SCHEMA_VERSION = 1;

/** A JSON-RPC error answer. */
export class RpcError extends Error {
  code: number;
  data: unknown;
  constructor(code: number, message: string, data?: unknown) {
    super(`${message} (code ${code})`);
    this.name = 'RpcError';
    this.code = code;
    this.data = data;
  }
}

interface Pending {
  resolve: (value: unknown) => void;
  reject: (reason: Error) => void;
}

export interface ConnectOptions {
  /** Abort socket connection and the hello handshake. */
  signal?: AbortSignal;
  /** Most events kept for `events()`; the oldest are dropped past this. Default 10000. */
  maxBufferedEvents?: number;
}

/** One connection to an emulator. Requests may overlap; replies are matched by id. */
export class Client {
  #socket: net.Socket;
  #nextId = 1;
  #pending = new Map<number, Pending>();
  #text = '';
  #queue: EmuEvent[] = [];
  #waiter: (() => void) | null = null;
  #closed: Error | null = null;
  #max: number;
  /** Events dropped because the buffer was full. */
  dropped = 0;

  constructor(socket: net.Socket, options: ConnectOptions = {}) {
    this.#socket = socket;
    this.#max = options.maxBufferedEvents ?? 10000;
    socket.setEncoding('utf8');
    socket.on('data', (chunk: string) => this.#onData(chunk));
    socket.on('close', () => this.#onClose(new Error('emulator closed the connection')));
    socket.on('error', (err) => this.#onClose(err));
  }

  #onData(chunk: string): void {
    this.#text += chunk;
    for (;;) {
      const nl = this.#text.indexOf('\n');
      if (nl < 0) return;
      const line = this.#text.slice(0, nl);
      this.#text = this.#text.slice(nl + 1);
      if (line.trim() !== '') this.#onMessage(JSON.parse(line));
    }
  }

  #onMessage(msg: {
    id?: number;
    method?: string;
    params?: EmuEvent;
    result?: unknown;
    error?: { code: number; message: string; data?: unknown };
  }): void {
    if (msg.method === 'event' && msg.params) {
      if (this.#queue.length >= this.#max) {
        this.#queue.shift();
        this.dropped++;
      }
      this.#queue.push(msg.params);
      this.#wake();
      return;
    }
    const pending = msg.id === undefined ? undefined : this.#pending.get(msg.id);
    if (!pending || msg.id === undefined) return;
    this.#pending.delete(msg.id);
    if (msg.error) pending.reject(new RpcError(msg.error.code, msg.error.message, msg.error.data));
    else pending.resolve(msg.result);
  }

  #onClose(err: Error): void {
    if (this.#closed) return;
    this.#closed = err;
    for (const p of this.#pending.values()) p.reject(err);
    this.#pending.clear();
    this.#wake();
  }

  #wake(): void {
    const w = this.#waiter;
    this.#waiter = null;
    w?.();
  }

  call<T = unknown>(method: string, params: object = {}): Promise<T> {
    if (this.#closed) return Promise.reject(this.#closed);
    const id = this.#nextId++;
    return new Promise<T>((resolve, reject) => {
      this.#pending.set(id, { resolve: resolve as (v: unknown) => void, reject });
      this.#socket.write(`${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`);
    });
  }

  /** Events received so far (after `eventsSubscribe`), oldest first; empties the buffer. */
  drainEvents(): EmuEvent[] {
    const out = this.#queue;
    this.#queue = [];
    return out;
  }

  /** Async iterator over pushed events; ends when the connection closes or `signal` aborts.
   * A `lagged` event from the daemon is yielded like any other. */
  async *events(signal?: AbortSignal): AsyncGenerator<EmuEvent> {
    for (;;) {
      const next = this.#queue.shift();
      if (next) {
        yield next;
        continue;
      }
      if (this.#closed || signal?.aborted) return;
      await new Promise<void>((resolve) => {
        this.#waiter = resolve;
        signal?.addEventListener('abort', () => resolve(), { once: true });
      });
    }
  }

  // ----- typed wrappers ------------------------------------------------------

  async hello(): Promise<{ schema_version: number; [key: string]: unknown }> {
    const r = await this.call<{ schema_version: number }>('hello', { min_schema: SCHEMA_VERSION });
    if (r.schema_version < SCHEMA_VERSION) {
      throw new RpcError(-32602, `host schema ${r.schema_version} is too old`);
    }
    return r;
  }
  status(): Promise<StatusResult> {
    return this.call('status');
  }
  firmwareLoad(params: FirmwareLoadParams): Promise<FirmwareLoadResult> {
    return this.call('firmware.load', params);
  }
  restart(autoNo = false): Promise<Record<string, unknown>> {
    return this.call('restart', autoNo ? { auto_no: true } : {});
  }
  step(instructions: number, extra: { timeout_wall_s?: number } = {}): Promise<Record<string, unknown>> {
    return this.call('step', { instructions, ...extra });
  }
  runUntil(params: RunUntilParams): Promise<Record<string, unknown>> {
    return this.call('run_until', params);
  }
  inputButton(code: number, down = true, atInstr?: number): Promise<unknown> {
    return this.call('input.button', opt({ code, down, at_instr: atInstr }));
  }
  inputTurn(encoder: number, detents = 1, atInstr?: number): Promise<unknown> {
    return this.call('input.turn', opt({ encoder, detents, at_instr: atInstr }));
  }
  inputTap(code: number, hold = 40_000_000, atInstr?: number): Promise<unknown> {
    return this.call('input.tap', opt({ code, hold, at_instr: atInstr }));
  }
  inputSchedule(events: InputScheduleEvent[]): Promise<unknown> {
    return this.call('input.schedule', { events });
  }
  framebufferGet(params: FramebufferParams = {}): Promise<FramebufferResult> {
    return this.call('framebuffer.get', params);
  }
  /** Hex SHA-256 of the 1024 decoded OLED bytes, without transferring them. */
  async frameHash(): Promise<string> {
    const r = await this.framebufferGet({ hash: true, data: false });
    if (!r.sha256) throw new Error('framebuffer.get returned no sha256');
    return r.sha256;
  }
  ledsGet(): Promise<LedsResult> {
    return this.call('leds.get');
  }
  eventsSubscribe(kinds: string[] = []): Promise<unknown> {
    return this.call('events.subscribe', kinds.length ? { kinds } : {});
  }
  cardLoad(dir: string): Promise<{ card: string; [key: string]: unknown }> {
    return this.call('card.load', { dir });
  }
  /** `stallMs` sets the stall threshold in emulated ms (0 disables); omit to read. */
  watchdogConfig(stallMs?: number): Promise<WatchdogResult> {
    return this.call('watchdog.config', opt({ stall_ms: stallMs }));
  }
  /** One guest longword, null when unmapped. */
  async memPeek(addr: number): Promise<number | null> {
    const r = await this.call<{ value: number | null }>('mem.cf.peek', { addr });
    return r.value === null ? null : Number(r.value);
  }
  guardSet(params: GuardSetParams): Promise<unknown> {
    return this.call('guard.set', params);
  }
  guardReport(): Promise<GuardReport> {
    return this.call('guard.report');
  }
  guardClear(): Promise<unknown> {
    return this.call('guard.clear');
  }
  /** Set the ColdFire PC probe; kept for every runtime installed later, so set before `firmware.load` or `restart` for a probe from reset. */
  pcProbeSet(params: PcProbeSetParams): Promise<{ probes: number }> {
    return this.call('pc.probe.set', params);
  }
  pcProbeReport(): Promise<PcProbeReport> {
    return this.call('pc.probe.report');
  }
  pcProbeClear(): Promise<unknown> {
    return this.call('pc.probe.clear');
  }
  quit(): Promise<unknown> {
    return this.call('quit');
  }

  /** Close the socket (does not stop the daemon). */
  close(): void {
    this.#socket.destroy();
  }
}

function opt(params: Record<string, unknown>): Record<string, unknown> {
  return Object.fromEntries(Object.entries(params).filter(([, v]) => v !== undefined));
}

/** Connect to a control socket and run the `hello` handshake. */
export function connect(socketPath: string, options: ConnectOptions = {}): Promise<Client> {
  return new Promise((resolve, reject) => {
    const socket = net.createConnection({ path: socketPath, signal: options.signal });
    socket.once('error', reject);
    socket.once('connect', () => {
      socket.off('error', reject);
      const client = new Client(socket, options);
      client.hello().then(
        () => resolve(client),
        (err) => {
          client.close();
          reject(err);
        },
      );
    });
  });
}
