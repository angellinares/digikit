import { createHash } from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { DkSession, EmuUsbTransport, frame, parseReply } from '@digi/dk-session';
import { resolveInputs, type Device, type RuntimeBaseline, type Session } from '@digi/test-support';
import { describe, expect } from 'vitest';
import { expectNoGuardHits, withReusablePatchedSession, type PatchedSession, type RuntimeCase } from '../support.ts';
import {
  bootInfo,
  bootInfoLoaded,
  buildCard,
  cardView,
  pokeKnob,
  selRecords,
  storeList,
  type CardOptions,
  type SelRecord,
} from './card-kit.ts';
import { cachedCard, cachedCardId, cardKey, fileSha } from './card-cache.ts';
import { uploadStore } from './device-data.ts';
import { advance, bounded, options, rpc } from './fx-live.ts';
import { withPausedFxProtocol } from './fx-session.ts';
import { record } from './artifacts.ts';
import { DEVICES, FIRMWARE, FULL_TIER, claimRunDir } from './devices.ts';
import { moduleArtifact } from './install.ts';
import { expectRuntimeUi } from './ready.ts';
import { RING_SYMBOLS } from './runtime-menu.ts';
import {
  planSymbol,
  prepareRuntimePlanOffline,
  validateRuntimePlan,
  type PreparedRuntimePlan,
  type RuntimePlan,
} from './runtime-plan.ts';
import {
  cardAfter,
  cardOverlay,
  linkCard,
  overlayFile,
  overlayRecords,
  overlayWrites,
  runtimeSafety,
  runtimeWatchEnv,
  withoutOwn,
} from './write-safety.ts';

/**
 * The one card fixture of the runtime device tests (settings, round trip, equiv, debug, card, crash, safe mode and upload): an owned daemon
 * that cold-boots one card image after the other, with the write watch of the runtime proof. A test boots a card (`boot`, `load`, `pausedBoot`),
 * steps or pauses as it needs, and reads what the guest left (`after`, `records`, `storeWrites`). When the daemon has closed, the write watch,
 * the flush check and the card writes of every case are checked (Python `common_checks`, or `common_checks_upload` with `uploads`).
 */

/** The runtime image of the Python scenarios (`B.RT`). */
export const RT = ['core/runtime', 'core/dynreg', 'core/dyndsp'];
/** Runtime variables the card scenarios read or poke (`DKRT_BOOT` is always planned). */
export const CARD_SYMBOLS = ['DKRT_CARD', 'DKRT_CRASH', 'DKRT_MAILBOX', 'DKRT_STATE', ...RING_SYMBOLS];
/** The test knob `DKRT_STORE_CAP` keeps every sector from here to header B out of the allocations (a DN2 store of 20 GiB then fits INFO's 32 bits). */
export const CAP_SECTORS = 0x40000;

/** A card: `modules` are module ids (stored as modules/<last>.dkm) or `{ name, file }` store entries; the rest are the `buildCard` options. */
export type CardSpec = Omit<CardOptions, 'dir' | 'modules'> & { modules?: (string | { name: string; file: string })[] };

export interface CardRigOptions {
  devices?: readonly Device[];
  /** Full tier only. */
  full?: boolean;
  /** Skip with this reason (the Python scenario still covers it); see the scenario-migration handover. */
  skip?: Partial<Record<Device, string>>;
  /** Module roots of the runtime image (`patchkit apply --modules`); omit for the emulator patch set. */
  roots?: string[];
  /** Plan symbols besides `DKRT_BOOT`. */
  symbols?: string[];
  /** Daemon environment besides the write watch; it overrides the watch's variables. A function gets the prepared runtime plan when the daemon starts. */
  env?: Record<string, string> | ((prepared: PreparedRuntimePlan) => Record<string, string>);
  /** ColdFire-only daemon ('zero'; 'cold': also without a snapshot) or with the SHARC (default). */
  dsp?: 'zero' | 'sharc' | 'cold';
  /** The first card: a function that gives its spec (default a blank store card) or `small`, a blank 1 MiB file (a test sets `DIGI_CARD_IDENTITY=small`). */
  initial?: 'small' | (() => CardSpec);
  /** Seed every card with the stock firmware's project (Python `Env.card`). */
  project?: boolean;
  /** Uploads and project writes are part of the scenario: the card writes may be the store range and the firmware project sectors only (`common_checks_upload`). */
  uploads?: boolean;
  /** Store sectors besides the two selection sectors that the test writes on purpose (Python `store_extra`). */
  storeExtra?: number[];
  /** The DT2 file system check never runs on a card that the firmware saved a project on. */
  relaxFs?: boolean;
  /** Boot paused at the ready point and settle by stepping (Python `settle_ms` of 300 ms and `wait_dsp`), so a test that times events in DSP frames starts from the same frame in every run. */
  hold?: boolean;
  /** A CPU exception is part of the scenario: faults of the session are not an error. */
  faults?: boolean;
  /** Stall threshold of the daemon in emulated ms (default 1000): for a scenario whose single guest request blocks the main loop longer. */
  stallMs?: number;
  /** More checks after the daemon has closed. */
  onClose?: (rig: CardRig, report: ReturnType<typeof runtimeSafety>) => void;
}

export interface CardRig {
  readonly device: Device;
  readonly plan: RuntimePlan;
  /** The artifact directory of the daemon. */
  readonly artifacts: string;
  /** The case of the card booted last. */
  readonly runtime: RuntimeCase;
  readonly session: Session;
  /** The image file of the card booted last. */
  readonly cardImage: string;
  /** The module store in absolute card sectors. */
  readonly store: ReturnType<typeof uploadStore>;
  /** The exported overlay of every finished case. */
  readonly overlays: readonly string[];
  /** The protocol session of the last `pausedBoot`. */
  readonly dk: DkSession;
  /** A module for a card: store path `modules/<as>.dkm` (default the last part of `id`) and its prepared .dkm. */
  mod(id: string, as?: string): { name: string; file: string };
  /** A file below `cfg/` for a card (the fault and crash modules read their delay from it). */
  cfg(name: string, bytes: Uint8Array): { name: string; file: string };
  /** A card in a fresh directory of the run directory (plusdrive build + the records of `spec`). */
  card(spec?: CardSpec): Promise<string>;
  /** Boot the runtime on this card image in the same daemon; the previous case's evidence is collected first. */
  boot(cardImage: string, o?: { deliverExceptions?: boolean }): Promise<RuntimeCase>;
  /** `boot` of a card built from `spec`, then `settle`. */
  load(tag: string, spec: CardSpec): Promise<RuntimeCase>;
  /** The first case (the `initial` card), settled. */
  first(): Promise<RuntimeCase>;
  /** Wait for the runtime's UI and pause (the test steps time itself). */
  settle(
    runtime: PatchedSession & { epoch?: number },
    afterEpoch?: number,
    options?: { timeout?: number },
  ): Promise<PatchedSession>;
  /** Poll until the UI loop was reached (and the boot is safe mode when `safe`); returns the boot's fields. */
  ready(safe?: boolean): Promise<Awaited<ReturnType<typeof bootInfo>>>;
  /** A protocol restart (the card and its guest writes stay); returns the runtime of the new boot, settled. */
  restart(runtime: PatchedSession): Promise<PatchedSession>;
  /** Run protocol calls on the paused daemon (the transport steps the emulator). */
  usb<T>(runtime: PatchedSession, run: (dk: DkSession) => Promise<T>): Promise<T>;
  /** The card the guest left: `base` (default the card booted last) with the overlay of its writes laid over a copy (Python `card_after`). */
  after(tag?: string, base?: string): Promise<string>;
  /** The two selection record copies as the card holds them now. */
  records(): Promise<(SelRecord | null)[]>;
  /** The sectors of the store that the guest has written, sorted. */
  storeWrites(): Promise<number[]>;
  /** All sectors the guest has written in this boot. */
  writes(tag: string): Promise<Set<number>>;
  /** Allow store sectors in the lifetime check (a test that writes them on purpose). */
  allowStoreWrites(sectors: Iterable<number>): void;
  /** Step until the guest soft-resets and the cold boot is ready (Python `Rig.wait_reset`). */
  waitReset(timeoutMs?: number): Promise<void>;

  // ---- paused protocol sessions (the upload tests) ----
  /** Cold boot on a card of its own that shares the image of `card`, wait for the main UI, pause and settle 300 ms; returns a protocol session that steps the emulator while it waits for a reply. */
  pausedBoot(card: string): Promise<DkSession>;
  /** The files of the store as the plusdrive reader reads them from the card after the guest's writes. */
  view(tag: string): ReturnType<typeof cardView>;
  /** `plusdrive store ls` of the card after the guest's writes. */
  ls(tag: string): ReturnType<typeof storeList>;
  /** Take a store of more than 4 GiB down to `CAP_SECTORS` on this boot (DKRT_STORE_CAP). */
  applyCap(): Promise<void>;
  /** INFO's free bytes of a card with no store: used 1, room for a one-entry table (cut by the cap). */
  freeFresh(): number;
  /** One request and its reply, whatever the status. `version` sends the request raw with that protocol version byte. */
  request(cmd: number, body?: Uint8Array, version?: number): Promise<{ status: number; body: Uint8Array }>;
  /** The status of one request. */
  status(cmd: number, body?: Uint8Array, version?: number): Promise<number>;
  /** One request that must be answered with status 0. */
  send(cmd: number, body?: Uint8Array): Promise<void>;
}

/** One describe per device (`label` in its title) with a rig inside. */
export function cardRigs(label: string, o: CardRigOptions, define: (rig: CardRig) => void): void {
  for (const device of o.devices ?? DEVICES) {
    describe.skipIf((o.full === true && !FULL_TIER) || o.skip?.[device] !== undefined)(
      `${device.toUpperCase()} ${FIRMWARE[device]}: ${label}`,
      () => {
        define(cardRig(device, label, o));
      },
    );
  }
}

/**
 * One daemon with the write-watch environment of the runtime proof, to be called inside a describe block: every test case starts on the first card
 * and loads its own cards in the same daemon. After the daemon closes, every sequenced watch log and every exported overlay is checked.
 */
export function cardRig(device: Device, scope: string, o: CardRigOptions = {}): CardRig {
  let prepared: PreparedRuntimePlan;
  let artifacts = '';
  let watchOutput = '';
  let image = '';
  let clone = '';
  let closed = 0;
  let seq = 0;
  let cap: number | undefined;
  let stepping: { dk: DkSession; transport: EmuUsbTransport } | undefined;
  const overlays: string[] = [];
  const epochs = new Set<number>();
  const extra = new Set(o.storeExtra ?? []);
  const symbols = ['DKRT_BOOT', ...(o.symbols ?? [])];

  const baseline = (cardImage: string, deliverExceptions?: boolean): RuntimeBaseline => {
    if (o.roots && !prepared.preparedImage)
      throw new Error('the explicit runtime plan lacks a prepared image identity');
    const boot = planSymbol(prepared.plan, 'DKRT_BOOT');
    return {
      cardImage,
      preparedImage: prepared.preparedImage,
      guard: { protect: [{ lo: boot, hi: boot + 8, label: 'runtime boot identity' }], allow_pc: [] },
      deliverExceptions,
      relaxedReady: o.relaxFs,
      paused: o.hold,
    };
  };
  const freshDir = (session: Session, tag = 'card') => {
    fs.mkdirSync(session.runDir, { recursive: true });
    return fs.mkdtempSync(path.join(session.runDir, `${tag}-`));
  };
  /** The overlay file of the session's card writes: by default only what the guest wrote since the load (`raw`: the daemon's whole overlay). */
  const exportOverlay = async (session: Session, tag: string, raw = false) => {
    const file = path.join(artifacts, `${tag}.overlay.bin`);
    await rpc(session, 'card.overlay', { path: file });
    if (!raw) fs.writeFileSync(file, withoutOwn(fs.readFileSync(file), cardOverlay(image)));
    return file;
  };
  const mod = (id: string, as?: string) => ({
    name: `modules/${as ?? id.split('/').at(-1)}.dkm`,
    file: moduleArtifact(device, id),
  });
  /**
   * A card of `spec`. It is built once into the card cache under a name of its inputs, so the same spec gives the same file in every run and
   * the daemon finds its snapshot. A card on a `base` outside the cache has no stable name: it is built in a fresh directory of the run dir.
   */
  const build = async (session: Session, tag: string, spec: CardSpec = {}) => {
    const { modules = [], name: _name, ...rest } = spec;
    const options = {
      ...(o.project ? { syx: resolveInputs(device, FIRMWARE[device]).syx } : {}),
      ...rest,
      modules: modules.map((m) => (typeof m === 'string' ? mod(m) : m)),
    };
    const base = options.base ? cachedCardId(options.base) : undefined;
    if (options.base && !base) return buildCard(device, { ...options, dir: freshDir(session, tag) });
    const key = cardKey(device, {
      ...options,
      syx: options.syx ? fileSha(options.syx) : undefined,
      modules: options.modules.map((m) => ({ name: m.name, sha: fileSha(m.file) })),
      base,
    });
    return cachedCard(key, (work) => buildCard(device, { ...options, dir: work, name: 'card' }));
  };

  const fixture = withReusablePatchedSession(
    {
      ...options(device),
      runDirClaim: () => claimRunDir(device),
      ...(o.stallMs !== undefined ? { stallMs: o.stallMs } : {}),
      dsp: o.initial === 'small' ? 'cold' : o.dsp,
      env: async () => {
        artifacts = fs.mkdtempSync(
          path.join(os.tmpdir(), `digi-${scope.split(':')[0]!.replace(/\W+/g, '-')}-${device}-`),
        );
        console.info('card scenario artifacts:', artifacts);
        prepared = await prepareRuntimePlanOffline(
          { device, firmware: FIRMWARE[device] },
          artifacts,
          artifacts,
          symbols,
          o.roots ? { modules: o.roots } : undefined,
        );
        watchOutput = path.join(artifacts, 'writewatch.jsonl');
        const env = runtimeWatchEnv(prepared.planFile, watchOutput);
        record(artifacts, 'watch-environment', env);
        return {
          ...env,
          ...(o.relaxFs ? { DIGI_COLD_RELAX_FS: '1' } : {}),
          ...(typeof o.env === 'function' ? o.env(prepared) : o.env),
        };
      },
    },
    {
      prepareBaseline: async (session) => {
        // A cold daemon (no firmware at spawn) has no emulator, so no watch summary, for its first epoch.
        if (o.initial !== 'small' && o.dsp !== 'cold') epochs.add((await session.status()).epoch as number);
        if (o.initial === 'small') {
          image = path.join(freshDir(session), 'small-card.img');
          fs.writeFileSync(image, Buffer.alloc(1 << 20)); // blank; the identity, not the file, sets the capacity
        } else image = await build(session, 'initial', o.initial?.());
        return baseline(image);
      },
      finishCase: async (runtime) => {
        const { session } = runtime;
        stepping?.dk.cancelPending();
        stepping = undefined;
        epochs.add(Number((await bounded(() => session.status())).epoch));
        overlays.push(await exportOverlay(session, `case-${++closed}`));
        await bounded(() => expectNoGuardHits(session));
        if (!o.faults) await bounded(() => session.expectNoFault(`case ${closed}`));
      },
      afterClose: async (session) => {
        const report = runtimeSafety(device, watchOutput, overlays, Math.max(1, epochs.size));
        record(artifacts, 'lifetime-safety', report);
        expect(report.write_events, 'write-watch on cf.expansion over every boot: no stray write').toBe(0);
        expect(report.flush_hits, 'flush check over every boot: no hit').toBe(0);
        // A blank small card is formatted by the stock firmware (writes below the store area): the test's own
        // "nothing written in the store range" check is the one that counts there, as in Python `run_card_small`.
        if (o.initial === 'small') {
          // no card-write policy
        } else if (o.uploads)
          for (const card of report.overlays)
            expect(
              card.outsideStore,
              `card writes of ${card.path}: the module store range and the firmware project sectors from 0x50000 only`,
            ).toEqual([]);
        else
          for (const card of report.overlays) {
            expect(
              card.invalid_store.filter((s) => !extra.has(s)),
              'card writes in the store range: only the two selection sectors and the sectors the scenario writes on purpose',
            ).toEqual([]);
            expect(card.invalid_low, 'card writes elsewhere: only the firmware project sectors from 0x50000').toEqual(
              [],
            );
          }
        o.onClose?.(rig, report);
        // The artifacts dir is ours. Keep it with the session dir when that is kept (a failed run).
        if (!fs.existsSync(session.runDir)) fs.rmSync(artifacts, { recursive: true, force: true });
      },
    },
    scope,
  );

  const settle: CardRig['settle'] = async (
    runtime,
    afterEpoch = o.relaxFs && runtime.epoch ? runtime.epoch - 1 : undefined,
    options = {},
  ) => {
    if (o.hold) {
      // Python `settle_ms` (SETTLE_MS, 300 emulated ms), then `wait_dsp`: up to 5 s until no module is still LOADING (its DSP part follows the ColdFire part)
      await advance(runtime.session, 300);
      await bootInfoLoaded(runtime.session, prepared.plan);
    }
    await expectRuntimeUi(runtime.session, prepared.plan, artifacts, {
      afterEpoch,
      paused: o.hold,
      timeout: options.timeout,
    });
    await rpc(runtime.session, 'run.mode', { mode: 'paused' });
    return runtime;
  };
  const afterCard = async (tag: string, base: string) => {
    const { session } = fixture.runtime;
    return cardAfter(base, await exportOverlay(session, tag, true), freshDir(session, 'after'));
  };
  const sectorsOf = (file: string) => Array.from(overlayRecords(fs.readFileSync(file)), (r) => r.sector);

  const rig: CardRig = {
    device,
    get plan() {
      return prepared.plan;
    },
    get artifacts() {
      return artifacts;
    },
    get runtime() {
      return fixture.runtime;
    },
    get session() {
      return fixture.runtime.session;
    },
    get cardImage() {
      return image;
    },
    get store() {
      return uploadStore(device);
    },
    get overlays() {
      return overlays;
    },
    get dk() {
      if (!stepping) throw new Error('no protocol session: boot with pausedBoot');
      return stepping.dk;
    },
    mod,
    cfg(name, bytes) {
      const file = path.join(artifacts, `cfg-${name}`);
      fs.writeFileSync(file, bytes);
      return { name: `cfg/${name}`, file };
    },
    card: (spec = {}) => build(fixture.runtime.session, spec.name ?? 'card', spec),
    async boot(cardImage, extraOptions = {}) {
      const next = await fixture.reload(baseline(cardImage, extraOptions.deliverExceptions));
      if (prepared.preparedImage) validateRuntimePlan(prepared.plan, next.session);
      epochs.add(next.epoch);
      image = cardImage;
      cap = undefined;
      return next;
    },
    async load(tag, spec) {
      const next = await rig.boot(await build(fixture.runtime.session, tag, spec));
      await settle(next);
      return next;
    },
    first: async () => (await settle(fixture.runtime)) as RuntimeCase,
    settle,
    async ready(safe = false) {
      const { session } = fixture.runtime;
      await expect
        .poll(
          async () => {
            const b = await bootInfo(session, prepared.plan);
            return b.uiReached === 1 && b.safe === (safe ? 1 : 0);
          },
          { timeout: 600_000, interval: 1000, message: `the UI loop was reached (safe mode: ${safe})` },
        )
        .toBe(true);
      return bootInfo(session, prepared.plan);
    },
    async restart(runtime) {
      const { session } = runtime;
      const before = Number((await bounded(() => session.status())).epoch);
      await rpc(session, 'run.mode', { mode: 'free' });
      await bounded(() => runtime.dk.restart());
      runtime.dk.cancelPending();
      const dk = new DkSession(new EmuUsbTransport((method, params) => session.client.call(method, params)));
      // This controller can answer throughout the guest reset: settle proves the fresh ready epoch/UI hook.
      cap = undefined;
      return settle({ session, dk, close: runtime.close }, before, { timeout: 90_000 });
    },
    usb: (runtime, run) => withPausedFxProtocol(runtime, run),
    after: (tag = `after-${++seq}`, base = image) => afterCard(tag, base),
    async records() {
      const { session } = fixture.runtime;
      return selRecords(session, image, device, await exportOverlay(session, `records-${++seq}`, true));
    },
    async storeWrites() {
      const { session } = fixture.runtime;
      return overlayWrites(await exportOverlay(session, `writes-${++seq}`), device).store_sectors.sort((a, b) => a - b);
    },
    async writes(tag) {
      return new Set(sectorsOf(await exportOverlay(fixture.runtime.session, tag)));
    },
    allowStoreWrites(sectors) {
      for (const s of sectors) extra.add(s);
    },
    async waitReset(timeoutMs = 900_000) {
      const { session } = fixture.runtime;
      const { client } = session;
      client.drainEvents();
      let resetSeen = false;
      const end = Date.now() + timeoutMs;
      while (Date.now() < end) {
        if (resetSeen) {
          // the controller notices that its worker has finished only when a command wakes it: a status request is one
          await delay(200);
          await session.status().catch(() => undefined);
        } else await client.step(400_000, { timeout_wall_s: 30 }).catch(() => delay(500)); // steps are refused during a cold boot
        for (const ev of client.drainEvents()) {
          if (ev.kind === 'cpu.reset') {
            if (ev.restart === 'unsupported') throw new Error('the emulator cannot restart this build');
            resetSeen = true;
          }
          if (ev.kind === 'firmware.loaded') {
            if (!ev.ok) throw new Error(`reset: the cold boot failed ${JSON.stringify(ev)}`);
            await client.watchdogConfig(session.options.stallMs ?? 1000);
            for (let i = 0; i < 200 && !(await session.status()).cf.ready; i++)
              await client.step(400_000, { timeout_wall_s: 30 });
            return;
          }
        }
      }
      throw new Error(`no reset within ${timeoutMs / 1000} s`);
    },

    async pausedBoot(card) {
      const { session } = fixture.runtime;
      const previous = clone && path.dirname(clone);
      // Named after the card (path and overlay), so a path never maps to two cards across runs.
      const id = createHash('sha256')
        .update(`${fs.realpathSync(card)}:${fs.existsSync(overlayFile(card)) ? fileSha(overlayFile(card)) : ''}`)
        .digest('hex')
        .slice(0, 12);
      const next = path.join(session.runDir, `boot-${id}`, 'card.img');
      fs.mkdirSync(path.dirname(next), { recursive: true });
      linkCard(card, path.dirname(next), cardOverlay(card)); // the daemon may save next to the image: boot a card of its own
      const runtime = await rig.boot(next);
      await expectRuntimeUi(runtime.session, prepared.plan, artifacts, { afterEpoch: runtime.epoch - 1 });
      await expect
        .poll(
          async () => {
            const status = await bounded(() => runtime.session.status());
            return { pending: status.cf.input_pending, scheduled: status.scheduled_inputs };
          },
          { timeout: 30_000, interval: 100, message: 'boot input has finished before pausing' },
        )
        .toEqual({ pending: 0, scheduled: 0 });
      await rpc(runtime.session, 'run.mode', { mode: 'paused' });
      await advance(runtime.session, 300);
      if (previous && previous !== path.dirname(next)) fs.rmSync(previous, { recursive: true, force: true });
      clone = next;
      const transport = new EmuUsbTransport((method, params) => runtime.session.client.call(method, params), {
        stepInstructions: 400_000,
      });
      stepping = { dk: new DkSession(transport, { timeoutMs: 60_000 }), transport };
      return stepping.dk;
    },
    async view(tag) {
      return cardView(await afterCard(tag, image), device);
    },
    async ls(tag) {
      return storeList(await afterCard(tag, image), device);
    },
    async applyCap() {
      const { limit } = rig.store;
      if (limit * 512 > 0xffffffff) {
        await pokeKnob(fixture.runtime.session, prepared.plan, 'DKRT_STORE_CAP', CAP_SECTORS);
        cap = CAP_SECTORS;
      }
    },
    freeFresh: () => Math.min(((cap ?? rig.store.limit) - 2) * 512, 0xffffffff),
    async request(cmd, body = new Uint8Array(), version) {
      if (!stepping) throw new Error('no protocol session: boot with pausedBoot');
      if (version === undefined) return stepping.dk.request(cmd, body);
      const seqNo = 0x55;
      await stepping.transport.send(frame(cmd, seqNo, body, version));
      for (;;) {
        const raw = await stepping.transport.recv(30_000);
        if (!raw) throw new Error('no reply to the raw request');
        const reply = parseReply(raw);
        if (reply && reply.seq === seqNo && reply.cmd === cmd) return reply;
      }
    },
    async status(cmd, body, version) {
      return (await rig.request(cmd, body, version)).status;
    },
    async send(cmd, body) {
      const status = await rig.status(cmd, body);
      if (status !== 0) throw new Error(`request ${cmd.toString(16)} answered ${status}`);
    },
  };
  return rig;
}
