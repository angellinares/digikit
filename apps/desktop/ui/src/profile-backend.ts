// What the Profile pane reads and does (ProfilePanel.tsx): the runtime's span counters of the debug module over the target's session (dk-session
// profileState / profileAll, PROFILE_STATE and PROFILE_READ), and on the emulator also `cf.profile` and `sharc.profile` of the controller (the
// Host's `rpc`). One recorder per panel: no module state; the session, the rpc, the library and dkprof.wasm are arguments, so a test gives fakes.
// The preview replaces this module (preview/profile-backend.mock.ts). Host tests: tests/profile-backend.test.ts.
import { CAP, LOG_KIND } from '@digi/dk-session/gen_proto';
import { DkError, type DkSession, type PollSlot } from '@digi/dk-session';
import type { DkProf } from '@digi/dk-session/dkprof';
import type { EmuRpc } from '@digi/workbench-core/host/host.ts';
import type { LibraryModule } from '@digi/workbench-core/modules/dktransfer.ts';
import {
  buildProfile,
  cfPart,
  runId,
  sharcPart,
  spansPart,
  type CfAnswer,
  type Part,
  type SharcAnswer,
} from '@digi/workbench-core/profile/profile-artifact.ts';
import {
  dspView,
  ownerName,
  type DspView,
  type ModuleRef,
  type OwnerKind,
  type PollInfo,
  type Snapshot,
} from '@digi/workbench-core/profile/profile-core.ts';
import type { LogNames } from '@digi/dk-session/log-names';

/** The part of the session the pane uses. */
export type ProfileSession = Pick<
  DkSession,
  'info' | 'modules' | 'capacity' | 'profileState' | 'profileAll' | 'logPage' | 'peek'
>;

/** The whole-block DSP load of a device, from its newest HEALTH event (percent of the block). */
export interface DeviceDsp {
  avgPct: number;
  maxPct: number;
}

/** One reading. `noDebug`: the target runs no debug module (or an older one without PROFILE_STATE): the pane offers to add it. */
export type ProfileReading =
  | { noDebug: true; port: string; emulator: boolean }
  | {
      noDebug: false;
      port: string;
      /** The target is the emulator (the controller's instruction profiles go with the spans). */
      emulator: boolean;
      snap: Snapshot;
      modules: ModuleRef[];
      /** Emulator: the modules' DSP per block (undefined when `sharc.profile` is off or the daemon lacks it). */
      dsp?: DspView;
      /** Hardware: the newest HEALTH event, when the ring holds one. */
      deviceDsp?: DeviceDsp;
      /** What the emulator's two instruction profiles could not give (the daemon lacks the feature), one line. */
      note?: string;
    };

export interface ProfileDeps {
  session: () => Promise<ProfileSession>;
  /** The controller's rpc: set when the target is the emulator (Start then also records `cf.profile` and `sharc.profile`). */
  rpc?: EmuRpc;
  loadLibrary: (device: string) => Promise<LibraryModule[]>;
  prof?: () => Promise<DkProf>;
  /** The maps and load table of the running image (the emulator's names), to name the DSP counters. */
  names?: Pick<LogNames, 'inputs' | 'refresh'>;
  now?: () => Date;
}

/** The debug module is not there: no PEEK capability, or PROFILE_STATE is an unknown command of an older debug module. */
export const isDebugMissing = (e: unknown): boolean => e instanceof DkError && e.status === 'E_PROTO';

const KINDS: Record<LibraryModule['group'], OwnerKind> = {
  Machines: 'machine',
  Effects: 'effect',
  Tools: 'tool',
  Services: 'tool',
};

const asFault = (e: unknown): string => (e instanceof Error ? e.message : String(e));

export interface Recorder {
  /** Read the state, the spans and the modules (and, on the emulator, the DSP per block). */
  read(): Promise<ProfileReading>;
  /** Reset the counters and start the spans; on the emulator also `cf.profile` and `sharc.profile` count from now. */
  start(): Promise<void>;
  /** Stop the spans; on the emulator, keep the instruction profiles of the recording for Export. */
  stop(): Promise<void>;
  /** Zero the counters (both states); the recording window starts again. */
  reset(): Promise<void>;
  /** The `dk.profile/1` of what was recorded (spans, and on the emulator cf.profile and sharc.profile), as the file text and a file name. */
  exportFile(): Promise<{ text: string; name: string }>;
}

export function createRecorder(deps: ProfileDeps): Recorder {
  const now = deps.now ?? (() => new Date());
  let library: { port: string; modules: LibraryModule[] } | undefined;
  let device = 'unknown';
  let cfBase: CfAnswer | undefined;
  /** The instruction profiles of the last recording, kept at Stop. */
  let kept: { cf?: Part; sharc?: Part } = {};
  let counting = false; // the emulator counts cf.profile and sharc.profile for a recording
  let instructionNote: string | undefined;

  const libraryOf = async (port: string): Promise<LibraryModule[]> => {
    if (library?.port !== port) {
      let modules: LibraryModule[] = [];
      try {
        modules = await deps.loadLibrary(port);
      } catch {
        // no library folder or no needs reader: the modules are not told apart
      }
      library = { port, modules };
    }
    return library.modules;
  };

  /** The poll slots with the function names of the address maps (the Log pane's maps and load table); a slot without a name keeps its address only. */
  const pollInfos = async (slots: (PollSlot | undefined)[]): Promise<(PollInfo | undefined)[]> => {
    const polls: (PollInfo | undefined)[] = slots.map((p) => (p ? { owner: p.owner, fn: p.fn } : undefined));
    const inputs = deps.names?.inputs();
    const used = polls.flatMap((p) => (p ? [p] : []));
    if (!deps.prof || !inputs || used.length === 0) return polls;
    try {
      const { results } = (await deps.prof()).addrLookup({
        maps: inputs.maps,
        loads: inputs.loads,
        addrs: used.map((p) => ({ cpu: 'cf' as const, addr: p.fn })),
      });
      used.forEach((p, i) => {
        const r = results[i];
        if (r?.text) p.name = r.text;
      });
    } catch {
      // no names: the rows say the owner only
    }
    return polls;
  };

  const moduleRefs = async (s: ProfileSession, port: string): Promise<ModuleRef[]> => {
    const lib = await libraryOf(port);
    return (await s.modules()).map((m) => ({
      index: m.index,
      name: m.name,
      kind: KINDS[lib.find((l) => l.name === m.name)?.group as LibraryModule['group']] ?? 'other',
    }));
  };

  const call = <T>(method: string, params?: Record<string, unknown>): Promise<T> => {
    if (!deps.rpc) return Promise.reject(new Error('no emulator'));
    return deps.rpc.call<T>(method, params);
  };

  /** The instruction profiles of the emulator since the start; a daemon without a feature answers an error, said once in `note`. */
  async function instructionParts(): Promise<{ cf?: Part; sharc?: Part }> {
    if (!deps.rpc) return {};
    const out: { cf?: Part; sharc?: Part } = {};
    const missing: string[] = [];
    try {
      out.cf = cfPart(cfBase, await call<CfAnswer>('cf.profile'));
    } catch {
      missing.push('cf.profile');
    }
    try {
      out.sharc = sharcPart(await call<SharcAnswer>('sharc.profile'));
    } catch {
      missing.push('sharc.profile');
    }
    instructionNote = missing.length
      ? `${missing.join(' and ')} not available: the emulator daemon was built without it (docs/CONTROL.md).`
      : undefined;
    return out;
  }

  async function dspOf(s: ProfileSession, modules: ModuleRef[], sharc: Part | undefined): Promise<DspView | undefined> {
    if (!sharc) return undefined;
    const blocks = sharc.metrics.find((m) => m.name === 'sharc.blocks')?.value ?? 0;
    if (!(blocks > 0)) return undefined;
    let headroom: number | undefined;
    try {
      headroom = (await s.capacity()).DSP_COST?.limit || undefined;
    } catch {
      // the device does not report it: the bar is relative to the total
    }
    let report: Parameters<typeof dspView>[0] = { blocks, series: [] };
    const inputs = deps.names?.inputs();
    if (deps.prof && inputs) {
      const doc = buildProfile({ run: { id: runId('emu', now()), target: device }, sharc });
      report = (await deps.prof()).profileReport({
        profile: doc,
        maps: inputs.maps,
        loads: inputs.loads,
        top: 40,
      }) as typeof report;
    }
    return dspView(report, modules, headroom);
  }

  /** The newest HEALTH event of the ring (the whole-block DSP load the runtime logs), or undefined. */
  async function health(s: ProfileSession): Promise<DeviceDsp | undefined> {
    try {
      const [oldest, newest, first] = await s.logPage(0);
      let events = first;
      if (newest > oldest + first.length) events = (await s.logPage(Math.max(oldest, newest - 26)))[2];
      const h = [...events].reverse().find((e) => e.kind === LOG_KIND.HEALTH && ((e.args[0]! >>> 16) & 4) !== 0);
      return h ? { avgPct: h.args[1]! / 10, maxPct: h.args[2]! / 10 } : undefined;
    } catch {
      return undefined;
    }
  }

  return {
    async read() {
      const s = await deps.session();
      const info = await s.info();
      if (info.caps === null || ((info.caps >>> CAP.PEEK) & 1) === 0)
        return { noDebug: true, port: info.port ?? '', emulator: !!deps.rpc };
      const port = info.port ?? '';
      device = port.split('-')[0] ?? 'unknown';
      let all;
      try {
        all = await s.profileAll();
      } catch (e) {
        if (isDebugMissing(e)) return { noDebug: true, port, emulator: !!deps.rpc };
        throw e;
      }
      const modules = await moduleRefs(s, port);
      const snap: Snapshot = {
        running: all.running,
        hz: all.hz,
        elapsedTicks: all.elapsedTicks,
        spans: all.spans,
        polls: await pollInfos(all.polls),
      };
      const r: ProfileReading = { noDebug: false, port, emulator: !!deps.rpc, snap, modules };
      if (deps.rpc) {
        if (counting && all.running) {
          const parts = await instructionParts();
          try {
            r.dsp = await dspOf(s, modules, parts.sharc);
          } catch {
            // the DSP section stays empty
          }
        }
        if (instructionNote) r.note = instructionNote;
      } else {
        const d = await health(s);
        if (d) r.deviceDsp = d;
      }
      return r;
    },

    async start() {
      const s = await deps.session();
      kept = {};
      await s.profileState('reset');
      if (deps.rpc) {
        counting = false;
        try {
          await deps.names?.refresh(s); // the maps of the image that runs now (a reload changes them)
        } catch {
          // no names: the DSP view has the total only
        }
        const missing: string[] = [];
        try {
          cfBase = await call<CfAnswer>('cf.profile');
        } catch {
          cfBase = undefined;
          missing.push('cf.profile');
        }
        try {
          await call('sharc.profile', { enable: true });
          counting = true;
        } catch {
          missing.push('sharc.profile');
        }
        counting ||= cfBase !== undefined;
        instructionNote = missing.length
          ? `${missing.join(' and ')} not available: the emulator daemon was built without it (docs/CONTROL.md).`
          : undefined;
      }
      await s.profileState('start');
    },

    async stop() {
      const s = await deps.session();
      await s.profileState('stop');
      if (deps.rpc && counting) {
        kept = await instructionParts();
        counting = false;
        try {
          await call('sharc.profile', { enable: false });
        } catch {
          // the daemon had nothing counting
        }
      }
    },

    async reset() {
      const s = await deps.session();
      await s.profileState('reset');
      kept = {};
      if (deps.rpc && counting) {
        try {
          cfBase = await call<CfAnswer>('cf.profile');
          await call('sharc.profile', { enable: true });
        } catch (e) {
          instructionNote = asFault(e);
        }
      }
    },

    async exportFile() {
      const s = await deps.session();
      const all = await s.profileAll();
      const info = await s.info();
      const modules = await moduleRefs(s, info.port ?? '');
      const parts = deps.rpc && counting ? await instructionParts() : kept;
      const polls = await pollInfos(all.polls);
      const spans = spansPart(all, (x) => ownerName(x, modules, polls));
      const id = runId(deps.rpc ? 'emu' : 'dev', now());
      const doc = buildProfile({
        run: { id, target: (info.port ?? 'unknown').split('-')[0] ?? 'unknown' },
        ...parts,
        spans,
      });
      return { text: `${JSON.stringify(doc)}\n`, name: `${id}.dkprof.json` };
    },
  };
}
