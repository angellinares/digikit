import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import {
  RpcError,
  connect,
  readSocket,
  spawnDaemon,
  type Client,
  type Daemon,
  type EmuEvent,
  type FirmwareLoadResult,
  type GuardSetParams,
  type InitialButton,
  type LedsResult,
  type StatusResult,
} from '@digi/control-client';
import {
  coupledEnv,
  repoRoot,
  resolveInputs,
  validateRuntimeInputs,
  validatePreparedRuntimeImage,
  type Device,
  type PreparedRuntimeImage,
} from './inputs.ts';
import { button, encoder } from './keys.ts';

/** Content-named card images that the device tests share between runs (tests/regression/runtime/card-cache.ts; `DIGI_CARD_CACHE` moves it for host tests). A baseline card may live here. */
export const cardCacheRoot = (): string =>
  process.env.DIGI_CARD_CACHE || path.join(repoRoot(), 'dist/session-cache/cards');

/** ColdFire instructions per emulated millisecond (the oracle clock). */
export const MS = 132_000;
/** A session refuses to start on a volume with less free space (card images and run dirs add up). */
const MIN_FREE_GB = 50;
const MAX_STEP = 200_000_000;
const FAULT_KINDS = new Set(['halt', 'fault', 'stall', 'stall.end', 'lagged']);

export type DspMode = 'zero' | 'sharc' | 'cold';

/**
 * The one place that decides the DSP mode. An explicit `dsp` wins. Otherwise a test runs without the DSP: the entry point
 * knows neither the modules a test will load nor what it reads, and the DSP is most of the boot time. A test that needs
 * DSP audio, DSP memory or a machine/FX module's DSP path passes `dsp: 'sharc'`. The session refuses audio and DSP RPCs
 * without a DSP (`DSP_ONLY_RPC`), so a wrongly classified test fails loudly. tools/patchkit/pk_devreload.py
 * `target_dsp_mode` is the module-list policy for the dev loop; module lists are not known here.
 */
export function resolveDsp(options: { dsp?: DspMode }): DspMode {
  return options.dsp ?? 'zero';
}

/** RPC methods that read audio, PCM or DSP state: without a DSP they would answer zeros or nothing. */
const DSP_ONLY_RPC = /^(pcm|dsp|audio)\./;

function needsDsp(what: string, dsp: DspMode): Error {
  return new Error(`${what} needs the DSP, but this session runs with dsp:'${dsp}'; pass dsp:'sharc' to this test`);
}

/** A client whose DSP-only RPCs throw when the session runs without a DSP. */
function guardDspClient(client: Client, dsp: DspMode): Client {
  if (dsp === 'sharc') return client;
  return new Proxy(client, {
    get(target, prop) {
      const value = Reflect.get(target, prop, target);
      if (prop !== 'call') return typeof value === 'function' ? value.bind(target) : value;
      return (method: string, ...rest: unknown[]) => {
        if (DSP_ONLY_RPC.test(method)) return Promise.reject(needsDsp(method, dsp));
        return (target.call as (...a: unknown[]) => unknown)(method, ...rest);
      };
    },
  }) as Client;
}

export interface SessionOptions {
  device: Device;
  firmware: string;
  /**
   * "zero": no DSP (`--dsp zero --cf-snapshot`); "sharc": coupled DSP audio from the ready inputs; "cold": no DSP and no
   * snapshot (every load boots cold, for a card identity other than the snapshot's).
   * Omitted: `resolveDsp` decides, here and nowhere else (see there).
   */
  dsp?: DspMode;
  /** Card sample directory for `card.load` (DT2). */
  card?: string;
  /** Run directory; default a fresh temporary directory. */
  runDir?: string;
  /** Shared store of booted-state snapshots (`DIGI_SNAPSHOT_DIR`); default `<runDir>/patched`, private to the run dir. */
  snapshotDir?: string;
  /** Mirror the module store under this session's run directory. */
  mirror?: boolean;
  /** Stall threshold in emulated ms for the watchdog (default 1000, as the Python harness). */
  stallMs?: number;
  binary?: string;
  /** Called as soon as the daemon is connected, before its first RPC (the load of a non-reused session): start a watchdog here. */
  onSession?: (session: Session) => void;
  /** Additional daemon environment, prepared before spawn. */
  env?: Record<string, string> | ((runDir: string) => Promise<Record<string, string>>);
  /** Press NO automatically when ready; defaults to true. */
  autoNo?: boolean;
  /** Runtime boot uses firmware.load's existing patch/cache path, then runs freely. Stock defaults stay stepped. */
  runtime?: { patchSet?: string; reuse?: boolean };
  /** Reusable coupled sessions only: configure the DSP inputs but leave firmware absent until the first baseline. */
  startEmpty?: boolean;
}

/** A pristine image in a fixture-owned child directory of Session.runDir; not a sample directory. */
export interface RuntimeBaseline {
  cardImage: string;
  /** Load this exact offline-patched artifact instead of asking the daemon to patch stock. */
  preparedImage?: PreparedRuntimeImage;
  /** Every caller must explicitly choose its scenario's guard policy, including an unguarded one. */
  guard: GuardSetParams;
  deliverExceptions?: boolean;
  /** Real logical panel transitions before cold execution; explicitly supplied (even empty) selects start:cold. */
  initialButtons?: readonly InitialButton[];
  /** Use the MainPanel contract with DIGI_COLD_RELAX_FS: missing filesystem verification is accepted, explicit failure is rejected. Stock verification/loading may continue. */
  relaxedReady?: boolean;
  /** Leave the emulator paused at the ready point (the default is free running); a test that times events in DSP frames steps from there. */
  paused?: boolean;
}

/**
 * A hang guard, not a deadline, for the phases before the daemon has a socket (environment preparation, the daemon's startup load) where the
 * no-progress watchdog cannot ask anything. A normal load takes 5-10 s; a healthy but slow host must never reach this.
 */
export const PRE_SOCKET_HANG_MS = 300_000;

/** Bound awaited control RPCs to one absolute lifecycle deadline, including a peer that never answers. */
export async function withinRuntimeDeadline<T>(deadline: number, operation: () => Promise<T>): Promise<T> {
  const remaining = deadline - Date.now();
  if (remaining <= 0) throw new Error('runtime lifecycle deadline exceeded');
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      operation(),
      new Promise<never>((_resolve, reject) => {
        timer = setTimeout(() => reject(new Error('runtime lifecycle deadline exceeded')), remaining);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

export class Session {
  readonly options: SessionOptions;
  #daemon: Daemon;
  #events: EmuEvent[] = [];
  #ownedDir: string | null;
  #load: FirmwareLoadResult | undefined;
  #written: string[] = [];
  #lastReload: { loadMs: number; totalMs: number } | undefined;
  #card: string | undefined;
  #preparedImage: PreparedRuntimeImage | undefined;
  #guarded: Client | undefined;
  #closing: Promise<void> | undefined;
  #reloading = false;
  #preserveDiagnostics = false;
  #watch: Promise<Client> | undefined;

  private constructor(options: SessionOptions, daemon: Daemon, ownedDir: string | null) {
    this.options = options;
    this.#daemon = daemon;
    this.#ownedDir = ownedDir;
  }

  /** The resolved DSP mode of this session. */
  get dsp(): DspMode {
    return this.options.dsp!;
  }

  get client(): Client {
    return (this.#guarded ??= guardDspClient(this.#daemon.client, this.dsp));
  }

  /** Throws unless the session has a DSP: for helpers that capture or read audio, PCM, spectra or DSP state. */
  requireDsp(what: string): void {
    if (this.dsp !== 'sharc') throw needsDsp(what, this.dsp);
  }

  /** Directory of this daemon, including its CLI discovery file. */
  get runDir(): string {
    return this.options.runDir ?? this.#ownedDir!;
  }

  /** Base card image loaded for this session; guest writes remain in its overlay. */
  get cardPath(): string | undefined {
    return this.#card;
  }

  /** Wall time of the last baseline reload: the `firmware.load` call alone, and the whole reload. */
  get lastReload(): { loadMs: number; totalMs: number } | undefined {
    return this.#lastReload;
  }

  /** Snapshot entries that this session's cold boots wrote into the store: removed when the file fails. */
  get writtenSnapshots(): readonly string[] {
    return this.#written;
  }

  get loadResult(): FirmwareLoadResult {
    if (!this.#load) throw new Error('firmware not loaded');
    return this.#load;
  }

  /** Published only after a baseline's exact artifact and loaded firmware identity have been checked. */
  get preparedRuntimeImage(): PreparedRuntimeImage | undefined {
    const image = this.#preparedImage;
    return image ? { ...image, modules: [...image.modules] } : undefined;
  }

  static async start(given: SessionOptions): Promise<Session> {
    const options: SessionOptions = { ...given, dsp: resolveDsp(given) };
    if (options.startEmpty && (options.dsp !== 'sharc' || !options.runtime?.reuse))
      throw new Error('empty startup requires a reusable coupled session');
    if (options.runtime?.reuse && options.mirror) throw new Error('reusable runtime sessions cannot mirror cards');
    const inp = resolveInputs(options.device, options.firmware);
    if (options.runtime) validateRuntimeInputs(inp);
    const volume = options.runDir && fs.existsSync(options.runDir) ? options.runDir : os.tmpdir();
    const { bavail, bsize } = fs.statfsSync(volume);
    const freeGb = (bavail * bsize) / 1e9;
    if (freeGb < MIN_FREE_GB) {
      throw new Error(
        `refusing to start: ${freeGb.toFixed(1)} GB free under ${volume}, need ${MIN_FREE_GB} GB (mise run clean:runs)`,
      );
    }
    const ownedDir = options.runDir ? null : fs.mkdtempSync(path.join(os.tmpdir(), 'digi-session-'));
    const runDir = options.runDir ?? ownedDir!;
    const args =
      options.dsp === 'cold'
        ? []
        : options.dsp === 'zero'
          ? [...(options.runtime ? [inp.syx] : []), '--dsp', 'zero', '--cf-snapshot', inp.cfSnapshot]
          : [
              ...(options.startEmpty ? [] : [inp.syx]),
              '--auto-coupled',
              '--audio-profile-syx',
              inp.syx,
              '--dsp-image',
              inp.dspImage,
              '--dsp-state',
              inp.dspState,
              '--cf-snapshot',
              inp.cfSnapshot,
              '--no-audio',
            ];
    if (options.runtime?.reuse) args.push('--no-card-sync');
    if (options.mirror) args.push('--mirror-dir', path.join(runDir, 'mirror'));
    // After the socket opens, the daemon and its load are watched through the no-progress watchdog (`onSession`); before it only the hang guards below apply.
    const controller = options.runtime ? new AbortController() : undefined;
    let session: Session | undefined;
    const started = (async () => {
      const prepareEnv = options.env;
      const extraEnv =
        typeof prepareEnv === 'function'
          ? await withinRuntimeDeadline(Date.now() + PRE_SOCKET_HANG_MS, () => prepareEnv(runDir)).catch((error) => {
              if (error instanceof Error && error.message === 'runtime lifecycle deadline exceeded')
                throw new Error(`environment preparation hung: no result within ${PRE_SOCKET_HANG_MS} ms`);
              throw error;
            })
          : prepareEnv;
      controller?.signal.throwIfAborted();
      const env = {
        ...(options.dsp === 'sharc' ? coupledEnv(inp.syx) : {}),
        ...(options.snapshotDir ? { DIGI_SNAPSHOT_DIR: options.snapshotDir } : {}),
        ...extraEnv,
      };
      const daemon = await spawnDaemon({
        binary: options.binary,
        args,
        env: Object.keys(env).length > 0 ? env : undefined,
        runDir,
        timeoutMs: PRE_SOCKET_HANG_MS, // hang guard: on expiry spawnDaemon kills its child and awaits its exit
        signal: controller?.signal,
      });
      session = new Session(options, daemon, ownedDir);
      options.onSession?.(session);
      controller?.signal.throwIfAborted();
      const checked = async <T>(operation: Promise<T>): Promise<T> => {
        const value = await operation;
        controller?.signal.throwIfAborted();
        return value;
      };
      const c = daemon.client;
      await checked(c.eventsSubscribe());
      // Reuse skips the redundant SDK patched boot. Empty startup additionally omits the CLI's stock load.
      // The first runtime identity/card is established by mandatory reloadRuntimeBaseline.
      if (options.runtime?.reuse) {
        if (options.startEmpty) {
          const status = await checked(c.status());
          if (status.firmware !== null || status.cf !== undefined)
            throw new Error('empty coupled startup unexpectedly loaded firmware');
        }
        return session;
      }
      let card: string | undefined;
      if (options.card) card = (await checked(c.cardLoad(options.card))).card;
      session.#card = card;
      session.#load = await checked(
        c.firmwareLoad({
          syx: inp.syx,
          start: options.runtime ? 'auto' : 'snapshot',
          ...(options.runtime ? { runtime: true, patch_set: options.runtime.patchSet } : {}),
          force: true,
          auto_no: options.autoNo ?? true,
          ...(card ? { card } : {}),
        }),
      );
      session.#touchCacheHit(session.#load);
      await checked(c.watchdogConfig(options.stallMs ?? 1000));
      if (options.runtime) await checked(c.call('run.mode', { mode: 'free' }));
      return session;
    })();
    try {
      return await started;
    } catch (err) {
      controller?.abort(err);
      if (session) {
        session.keepDiagnostics();
        await session.close();
      } else {
        // A spawn still connecting owns the child: its abort path waits for exit. If a handle
        // arrives after expiry, the signal check rejects and we close that late handle too.
        const lateSession = await started.catch(() => session);
        if (lateSession) await lateSession.close();
        else if (ownedDir) console.info(`kept run dir for diagnostics: ${ownedDir}`); // the daemon did not start: its stderr is there
      }
      throw err;
    }
  }

  // The boot cache is pruned by mtime (tests/regression/runtime/devices.ts cachedRunDir), so a hit marks its entry as recent.
  #touchCacheHit(load: { cache_hit?: boolean; cache_dir?: unknown }): void {
    if (typeof load.cache_dir !== 'string') return;
    if (load.cache_hit !== true) {
      this.#written.push(load.cache_dir);
      return;
    }
    try {
      const now = new Date();
      fs.utimesSync(load.cache_dir, now, now);
    } catch {
      /* A missing cache dir is not an error. */
    }
  }

  /** Clean independent runtime case in the owned process. Persistence cases must use protocol restart instead. */
  async reloadRuntimeBaseline(baseline: RuntimeBaseline): Promise<number> {
    if (!this.options.runtime?.reuse) throw new Error('runtime baseline reload requires runtime.reuse');
    if (this.#closing || this.#reloading) throw new Error('runtime session is closed or already reloading');
    this.#reloading = true;
    const reloadStart = performance.now();
    let loadMs = 0;
    let phase = 'validate.card';
    // No wall deadline: a reload that stops moving is closed by the no-progress watchdog, which fails each pending call.
    const checked = <T>(operationPhase: string, operation: () => Promise<T>) => {
      phase = operationPhase;
      return operation();
    };
    let lastStatus: StatusResult | undefined;
    let statusPhase: 'before-load' | 'after-load' | undefined;
    let resolvedLoad: FirmwareLoadResult | undefined;
    let expectedImage: PreparedRuntimeImage | undefined;
    try {
      const roots = [this.runDir, cardCacheRoot()]
        .filter((dir) => fs.existsSync(dir))
        .map((dir) => fs.realpathSync(dir));
      const directory = fs.realpathSync(path.dirname(baseline.cardImage));
      const image = fs.realpathSync(baseline.cardImage);
      const inside = (root: string, file: string) => {
        const relative = path.relative(root, file);
        return (
          Boolean(relative) && relative !== '..' && !relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative)
        );
      };
      if (!roots.some((root) => inside(root, directory) && inside(root, image))) {
        throw new Error('runtime baseline card must be in a fixture-owned child directory of runDir or the card cache');
      }
      // A card of the cache that a session loads is in use: eviction goes by the dir's mtime.
      if (fs.existsSync(cardCacheRoot()) && inside(fs.realpathSync(cardCacheRoot()), directory)) {
        const now = new Date();
        fs.utimesSync(directory, now, now);
      }
      // The card's own `overlay.bin` (its test writes) is part of the baseline: the daemon applies it on load.
      if (!fs.statSync(image).isFile()) throw new Error('runtime baseline must be a card image file');
      if (!baseline.guard) throw new Error('runtime baseline requires an explicit scenario guard policy');
      // The visible job can finish; a hidden patch job may still reject load as BUSY, which discards this daemon.
      let before = (lastStatus = await checked('status.before-load', () => this.client.status()));
      statusPhase = 'before-load';
      while (before.job) {
        await checked('job.wait', () => delay(100));
        before = lastStatus = await checked('status.job-wait', () => this.client.status());
      }
      phase = 'validate.epoch';
      const epoch = before.epoch;
      if (typeof epoch !== 'number') throw new Error('runtime status has no epoch');
      this.#load = undefined;
      this.#card = undefined;
      this.#preparedImage = undefined;
      this.client.drainEvents();
      this.#events = [];
      await checked('cpu.exceptions', () =>
        this.client.call('cpu.exceptions', { deliver: baseline.deliverExceptions ?? false }),
      );
      await checked('watchdog.config', () => this.client.watchdogConfig(this.options.stallMs ?? 1000));
      phase = 'prepare.inputs';
      const inp = resolveInputs(this.options.device, this.options.firmware);
      const prepared = baseline.preparedImage
        ? { ...baseline.preparedImage, modules: [...baseline.preparedImage.modules] }
        : undefined;
      expectedImage = prepared;
      phase = 'validate.prepared.before-load';
      if (prepared) validatePreparedRuntimeImage(prepared, inp.syx);
      // A paused baseline pauses before the load: the runtime is installed in the mode the daemon has (a new session runs free,
      // at full speed), so the DSP frames it runs between the install and the pause would count against a test's trigger frame.
      if (baseline.paused)
        await checked('run.mode.before-load', () => this.client.call('run.mode', { mode: 'paused' }));
      const loadStart = performance.now();
      const load = (resolvedLoad = await checked('firmware.load', () =>
        this.client.firmwareLoad({
          syx: prepared?.syx ?? inp.syx,
          card: image,
          ...(prepared
            ? { base_sha256: prepared.baseSha256 }
            : { runtime: true, patch_set: this.options.runtime!.patchSet }),
          // Prepared images need validated stock bytes for the DSP diff; an empty coupled daemon resolves its profile.
          // force still reconstructs the firmware and opens the explicit pristine card.
          replace: !prepared,
          force: true,
          start: baseline.initialButtons === undefined ? 'auto' : 'cold',
          initial_buttons: baseline.initialButtons,
          auto_no: this.options.autoNo ?? true,
        }),
      ));
      loadMs = performance.now() - loadStart;
      phase = 'cache.touch';
      this.#touchCacheHit(load);
      await checked('guard.set', () => this.client.guardSet(baseline.guard));
      await checked('run.mode.after-load', () =>
        this.client.call('run.mode', { mode: baseline.paused ? 'paused' : 'free' }),
      );
      const status = (lastStatus = await checked('status.after-load', () => this.client.status()));
      statusPhase = 'after-load';
      phase = 'validate.loaded';
      if (this.options.startEmpty && (status.dsp !== 'sharc' || status.audio == null))
        throw new Error('empty coupled baseline did not load coupled audio');
      if (
        typeof status.epoch !== 'number' ||
        status.epoch <= epoch ||
        !(baseline.relaxedReady
          ? status.cf.main_ui_reached && status.cf.filesystem_verified !== false
          : status.cf.ready) ||
        status.mode !== (baseline.paused ? 'paused' : 'free')
      ) {
        throw new Error(`runtime baseline did not establish a new ready epoch: ${JSON.stringify(status)}`);
      }
      phase = 'validate.prepared.after-load';
      if (prepared) {
        validatePreparedRuntimeImage(prepared, inp.syx);
        if (
          load.syx_sha256 !== prepared.syxSha256 ||
          status.firmware?.syx_sha256 !== prepared.syxSha256 ||
          status.firmware?.base_sha256 !== prepared.baseSha256
        ) {
          throw new Error(
            'prepared runtime loaded firmware identity does not match the exact artifact and stock image',
          );
        }
      }
      phase = 'commit.baseline';
      this.#load = load;
      this.#card = image;
      this.#preparedImage = prepared;
      this.#lastReload = { loadMs, totalMs: performance.now() - reloadStart };
      this.#collect();
      return status.epoch;
    } catch (error) {
      // A rejected or unanswered reset never becomes the baseline of a later test.
      this.#load = undefined;
      this.#card = undefined;
      this.#preparedImage = undefined;
      try {
        fs.writeFileSync(
          path.join(this.runDir, 'runtime-baseline-error.json'),
          JSON.stringify(
            {
              error: String(error),
              phase,
              cardImage: baseline.cardImage,
              preparedImage: expectedImage ?? null,
              load: resolvedLoad ?? null,
              status: lastStatus ?? null,
              statusPhase: statusPhase ?? null,
            },
            null,
            2,
          ),
        );
      } catch {
        /* Preserve the original reset failure even if its artifact cannot be written. */
      }
      console.info('runtime reset diagnostics:', this.runDir);
      await this.close({ preserveDiagnostics: true });
      throw error;
    } finally {
      this.#reloading = false;
    }
  }

  /** Back to the ready state of the running firmware, modal closed; clears collected events. */
  async restart(): Promise<void> {
    if (this.options.runtime) throw new Error('runtime sessions require a fresh daemon; restart is stock-only');
    await this.client.restart(true);
    this.#collect();
    this.#events = [];
    await this.wait(300);
  }

  /** Tap a key (or a chord: all but the last are held), hold HOLDMS, release. */
  async press(key: string | number | (string | number)[], holdMs = 50): Promise<void> {
    const seq = Array.isArray(key) ? key : [key];
    const last = seq[seq.length - 1];
    if (last === undefined) throw new Error('press: empty key list');
    const held = seq.slice(0, -1).map(button);
    for (const code of held) {
      await this.client.inputButton(code, true);
      await this.wait(5);
    }
    await this.client.inputTap(button(last), Math.floor(holdMs * MS));
    await this.wait(holdMs + 5);
    for (const code of held.reverse()) {
      await this.client.inputButton(code, false);
      await this.wait(5);
    }
  }

  async turn(enc: string | number, detents: number): Promise<void> {
    await this.client.inputTurn(encoder(enc), detents);
    await this.wait(10);
  }

  /** Run MS emulated milliseconds. A guest fault is recorded in `faults()` and rethrown. */
  async wait(ms: number): Promise<void> {
    if (this.options.runtime) throw new Error('runtime sessions run freely; wait() steps stock sessions only');
    let n = Math.floor(ms * MS);
    while (n > 0) {
      const chunk = Math.min(n, MAX_STEP);
      try {
        await this.client.step(chunk, { timeout_wall_s: 300 });
      } catch (err) {
        if (err instanceof RpcError && err.code === 6) {
          this.#collect();
          this.#events.push({ kind: 'fault', message: err.message, data: err.data });
        }
        throw err;
      }
      n -= chunk;
    }
  }

  /** Hex SHA-256 of the 1024 decoded OLED bytes. */
  frameHash(): Promise<string> {
    return this.client.frameHash();
  }
  leds(): Promise<LedsResult> {
    return this.client.ledsGet();
  }
  status(): Promise<StatusResult> {
    return this.client.status();
  }

  /** `status` over a second connection, for a watchdog: it answers while a `firmware.load` runs on the main connection. */
  async watchStatus(): Promise<StatusResult> {
    this.#watch ??= (async () => {
      const socket = readSocket(this.runDir);
      if (!socket) throw new Error('the daemon has no control socket');
      return connect(socket);
    })();
    try {
      return await (await this.#watch).status();
    } catch (error) {
      // A failed connect or a closed connection is opened again by the next call.
      void this.#watch?.then((client) => client.close()).catch(() => {});
      this.#watch = undefined;
      throw error;
    }
  }

  #collect(): void {
    this.#events.push(...this.client.drainEvents().filter((e) => e.kind !== 'frame'));
  }

  /** Fault, halt, stall, guard and lagged events collected since the last restart. */
  async faults(): Promise<EmuEvent[]> {
    await this.client.status(); // events can follow their reply; a round trip flushes them
    this.#collect();
    return this.#events.filter((e) => FAULT_KINDS.has(e.kind) || e.kind.startsWith('guard'));
  }

  /** Throws when `faults()` is non-empty or status shows a fault, DSP halt or stall. */
  async expectNoFault(where = ''): Promise<void> {
    const status = await this.client.status();
    const faults = await this.faults();
    const problems: string[] = faults.map((e) => JSON.stringify(e));
    const { cf, heartbeat } = status;
    if (cf.error || cf.phase === 'fault') problems.push(`cf fault: ${cf.error} pc=${cf.pc.toString(16)}`);
    if (status.audio?.halted) problems.push(`DSP halt: ${JSON.stringify(status.audio.halted)}`);
    if (heartbeat.stalled || heartbeat.stall_count) {
      problems.push(`stall: count=${heartbeat.stall_count} threshold=${heartbeat.stall_ms} ms`);
    }
    if (problems.length) throw new Error(`fault${where ? ` at ${where}` : ''}: ${problems.join('; ')}`);
  }

  /** A test, hook or check failed: `close` keeps the temporary run directory (if this session made one) and prints its path. */
  keepDiagnostics(): void {
    this.#preserveDiagnostics = true;
  }

  /** Quit the daemon and remove an owned temporary run directory, unless diagnostics are kept (`keepDiagnostics`, `preserveDiagnostics`). */
  close(options: { preserveDiagnostics?: boolean } = {}): Promise<void> {
    if (options.preserveDiagnostics) this.#preserveDiagnostics = true;
    return (this.#closing ??= (async () => {
      try {
        void this.#watch?.then((client) => client.close()).catch(() => {});
        await this.#daemon.close();
      } finally {
        // The one rule for a run dir that Session.start made itself: a clean close removes it, a failed one keeps it.
        if (this.#ownedDir && this.#preserveDiagnostics)
          console.info(`kept run dir for diagnostics: ${this.#ownedDir}`);
        else if (this.#ownedDir) fs.rmSync(this.#ownedDir, { recursive: true, force: true });
      }
    })());
  }
}
