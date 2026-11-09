// A fake runtime log for the Log drawer in the preview (?page=log&log=both|fault|frozen|empty|nodebug: a runtime without the debug module, so without the LOG capability). Each origin is a script of events on a clock:
// a boot, modules, MOD_LOG lines, then (log=both) a gap in the device's ring, a restart of the emulator, and a boot of the device that found a
// stopped DSP. The sessions answer as the follower needs them (log-follow-core.ts); the events are the runtime's (docs/findings/46-digikit-debug.md).
import type { DkLogEvent, Fault, LogConfig } from '@digi/dk-session';
import { CAP, LOG_KIND } from '@digi/dk-session/gen_proto';
import type { FollowSession } from '../src/log-follow-core';

type Mode = 'both' | 'fault' | 'frozen' | 'empty' | 'nodebug';
const query = new URLSearchParams(location.search);
export const logMode: Mode = (query.get('log') as Mode) ?? 'both';
/** How much faster than real time the script runs (a screenshot does not wait ten seconds). */
const SPEED = Number(query.get('logspeed') ?? 1);

const word = (s: string) => {
  let n = 0;
  for (let i = 0; i < 4; i++) n = n * 256 + (s.charCodeAt(i) || 0);
  return n;
};
const words = (s: string, n: number) => Array.from({ length: n }, (_, i) => word(s.slice(4 * i, 4 * i + 4)));
const lv = (level: number, src = 0, error = false) => (level << 1) | (src << 8) | (error ? 1 : 0);

interface Item {
  at: number;
  kind: keyof typeof LOG_KIND;
  args?: number[];
  flags?: number;
  /** Sequence numbers the ring lost before this event. */
  skip?: number;
}
/** One boot: the events with the time (ms from the start of the script) each is logged at. */
interface Boot {
  start: number;
  items: Item[];
}

const ev = (at: number, kind: keyof typeof LOG_KIND, args: number[] = [], flags = 0, skip = 0): Item => ({
  at,
  kind,
  args,
  flags,
  skip,
});
const modLog = (at: number, src: number, msg: string, value: number, skip = 0) =>
  ev(at, 'MOD_LOG', [...words(msg, 4), value], lv(2, src), skip);
const mod = (index: number, name: string) => [
  ev(0, 'MOD_LOAD', [index, 0, ...words(name, 3)], lv(2)),
  ev(0, 'MOD_LIST', [index, 3, ...words(name, 3)], lv(2)),
];
const boot = (device: number): Item[] => [
  ev(0, 'BOOT_START', [1, device, 450, 7, 1], lv(2)),
  ev(0, 'CARD_CHECK', [1, 0xffffffff, 0x0f498b0e], lv(2)),
];
const ticks = (src: number, msg: string, from: number, to: number, every: number): Item[] =>
  Array.from({ length: Math.floor((to - from) / every) + 1 }, (_, i) =>
    modLog(from + i * every, src, msg, 0x12c + i * 7),
  );

// the tail of a crash record: two CRASH_TAIL events for each event before the stop (seq, time, kind << 16 | flags, arg0, arg1; seq, arg2, arg3, arg4, 1)
const tail = (seq: number, blocks: number, kind: number, flags: number, a: number[]): Item[] => [
  ev(0, 'CRASH_TAIL', [seq, blocks, (kind << 16) | flags, a[0] ?? 0, a[1] ?? 0]),
  ev(0, 'CRASH_TAIL', [seq, a[2] ?? 0, a[3] ?? 0, a[4] ?? 0, 1]),
];

const deviceBoot0: Boot = {
  start: 0,
  items: [
    ...boot(2),
    ...mod(0, 'dyn-fx-hang'),
    ...mod(1, 'freqshift'),
    ev(300, 'UI_FIRST', [450, 0, 2], lv(2)),
    ev(600, 'STABLE', [900, 450], lv(2)),
    ev(1200, 'HEALTH', [((2 | 4) << 16) | 371, 120, 450, 100000, 2400], lv(3)),
    ...ticks(2, 'shift tick', 1000, 2500, 1000),
    // the ring lost 6 events here
    modLog(3500, 2, 'shift tick', 0x150, 6),
    ev(4500, 'MENU', [3, 1], lv(2)),
    modLog(5500, 2, 'shift tick', 0x15e),
    ev(6200, 'FX_FAULT', [1, 8], lv(0, 1, true)),
    ev(6900, 'DSP_STOPPED', [4021, 4020, 4 | (12 << 8) | (18 << 16), 220, 0], lv(0, 0, true)),
  ],
};
// the boot after the stop: safe mode (reason dsp), the fault found, and the events before the stop
const deviceBoot1: Boot = {
  start: 8000,
  items: [
    ...boot(2),
    ev(0, 'SAFE', [4, 2, 8], lv(0, 0, true)),
    ev(0, 'FAULT_FOUND', [2 | (3 << 8) | (4 << 16), 96, 3, 0x600304, 0], lv(0, 0, true)),
    ev(0, 'CRASH_MOD', [0, 0, ...words('dyn-fx-hang', 3)], lv(0, 0, true)),
    ev(0, 'DSP_DIAG', [(32 << 8) | 6, 0x600304, 3, 7, 9], lv(0, 0, true)),
    ...tail(88, 9800, LOG_KIND.FX_FAULT, 1, [1, 8, 0, 0, 0]),
    ...tail(89, 9900, LOG_KIND.MOD_LOG, lv(2, 2), [
      ...words('shif', 1),
      ...words('t ti', 1),
      ...words('ck', 1),
      0,
      0x15e,
    ]),
    ...tail(90, 10350, LOG_KIND.DSP_STOPPED, 1, [4021, 4020, 0, 220, 0]),
    ev(300, 'UI_FIRST', [450, 1, 0], lv(2)),
    ev(900, 'MENU', [2, 0], lv(2)),
  ],
};
const deviceFault: Fault = {
  kind: 'dsp',
  site: 3,
  stage: 4,
  id: 96,
  step: 3,
  at: 0x600304,
  runtime: 0x050300,
  module: 'test/dyn-fx-hang',
};

const emulatorBoot0: Boot = {
  start: 0,
  items: [
    ...boot(1),
    ...mod(0, 'freqshift'),
    ev(300, 'UI_FIRST', [450, 0, 1], lv(2)),
    ev(600, 'STABLE', [900, 450], lv(2)),
    ...ticks(1, 'shift tick', 1500, 5000, 1000),
    ev(2600, 'HEALTH', [((2 | 4) << 16) | 120, 40, 90, 120000, 4096], lv(3)),
  ],
};
const emulatorBoot1: Boot = {
  start: 6000,
  items: [
    ...boot(1),
    ...mod(0, 'freqshift'),
    ev(300, 'UI_FIRST', [450, 0, 1], lv(2)),
    ...ticks(1, 'shift tick', 1500, 4000, 1000),
  ],
};

interface Script {
  boots: Boot[];
  caps: number;
  debug: LogConfig;
  /** What MODULES answers: the modules loaded (the names the Log uses for the source of an event). */
  modules?: { index: number; name: string }[];
  /** The fault record the card holds after the boot with this index starts. */
  faults: (Fault | null)[];
  /** From this script time on, nothing answers. */
  silentFrom?: number;
}

/** The ring a script has at `ms`: the events of the latest boot, with sequence numbers from 1 (and the skipped ones left out). */
function ringAt(script: Script, ms: number): { events: DkLogEvent[]; boot: number } {
  let b = 0;
  script.boots.forEach((x, i) => {
    if (x.start <= ms) b = i;
  });
  const boots = script.boots[b]!;
  let seq = 0;
  const events: DkLogEvent[] = [];
  for (const item of boots.items) {
    seq += 1 + (item.skip ?? 0);
    if (boots.start + item.at > ms) break;
    events.push({
      seq,
      time: Math.round((item.at * 1500) / 1000),
      kind: LOG_KIND[item.kind],
      flags: item.flags ?? 0,
      args: [...(item.args ?? []), 0, 0, 0, 0, 0].slice(0, 5),
    });
  }
  return { events, boot: b };
}

function session(script: Script): FollowSession {
  const t0 = Date.now();
  const clock = () => (Date.now() - t0) * SPEED;
  const gate = async () => {
    if (script.silentFrom !== undefined && clock() >= script.silentFrom) {
      await new Promise((r) => setTimeout(r, 400));
      throw Object.assign(new Error('no reply from the device'), { kind: 'timeout' });
    }
  };
  return {
    async info() {
      await gate();
      return { caps: script.caps };
    },
    async logPage(from) {
      await gate();
      const { events } = ringAt(script, clock());
      const held = events.filter((e) => e.seq >= from);
      return [events[0]?.seq ?? 0, events.at(-1)?.seq ?? 0, held.slice(0, 8)];
    },
    async logConfig() {
      await gate();
      return script.debug;
    },
    // the fake runtime keeps the levels as the real one does (RAM, one state)
    async setLogConfig(level, modules) {
      await gate();
      script.debug = { ...script.debug, level, modules };
      return script.debug;
    },
    async modules() {
      await gate();
      return script.modules ?? [];
    },
    async fault() {
      await gate();
      return script.faults[ringAt(script, clock()).boot] ?? null;
    },
  };
}

// the capabilities while digikit/debug is loaded; without it INFO has neither LOG nor PEEK nor LOGCFG
const RUNTIME_CAPS = (1 << CAP.SERVICE_CMDS) | (1 << CAP.SETTINGS);
const LOG_CAPS = RUNTIME_CAPS | (1 << CAP.LOG) | (1 << CAP.PEEK) | (1 << CAP.LOGCFG);
const noDebug = logMode === 'nodebug';
const both = logMode === 'both';
const emulatorScript: Script = {
  boots: both ? [emulatorBoot0, emulatorBoot1] : [emulatorBoot0],
  caps: noDebug ? RUNTIME_CAPS : LOG_CAPS,
  debug: { level: 2, modules: Array<number>(8).fill(0xff), debug: true },
  modules: [{ index: 0, name: 'freqshift' }],
  faults: [],
};
const deviceScript: Script = {
  boots: logMode === 'fault' ? [{ ...deviceBoot1, start: 0 }] : both ? [deviceBoot0, deviceBoot1] : [deviceBoot0],
  caps: noDebug ? RUNTIME_CAPS : LOG_CAPS,
  debug: { level: 2, modules: [0xff, 3, ...Array<number>(6).fill(0xff)], debug: true },
  modules: [
    { index: 0, name: 'dyn-fx-hang' },
    { index: 1, name: 'freqshift' },
  ],
  faults: logMode === 'fault' || noDebug ? [deviceFault] : [null, deviceFault],
  ...(logMode === 'frozen' ? { silentFrom: 3000 } : {}),
};

export const previewLog = {
  emulatorRunning: () => logMode !== 'empty',
  devicePort: () => (logMode === 'empty' ? undefined : 'Elektron Digitakt II'),
  emulator: session(emulatorScript),
  device: session(deviceScript),
};
