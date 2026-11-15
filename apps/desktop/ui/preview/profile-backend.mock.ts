// The Profile pane's backend in the preview (replaces ../src/profile-backend.ts): a recorder over a made-up device, so the pane's own code draws
// the three states. The state is in the address: `prof=rec` (recording; the default), `prof=stopped` (a stopped target), `prof=nodebug` (no debug
// module); `card=dn2|dt2` names the device, and the Remote page (`page=remote`) reads a device, the Emulator page the emulator.
// The shares follow a fixed model per owner (a machine, an effect that is loaded after 19 s, the debug module, phases and polls); the clock is real
// time, one reading a second. The numbers show layout, not measurements. The DSP figures go through the app's own `dspView`.
import { dspView } from '@digi/workbench-core/profile/profile-core.ts';
import type { ModuleRef, Snapshot, SpanIn } from '@digi/workbench-core/profile/profile-core.ts';
import type { ProfileReading, Recorder } from '../src/profile-backend';

export { isDebugMissing } from '../src/profile-backend';

const query = new URLSearchParams(location.search);
const state = query.get('prof') ?? 'rec';
const dev = query.get('card') === 'dt2' ? 'dt2' : 'dn2';
const emulator = query.get('page') !== 'remote';
const HZ = 132_000_000;
const FX_AT = 19; // seconds after the start

interface Model {
  index: number;
  kind: SpanIn['kind'];
  /** The share of the CPU at second `t`, and the mean and maximum of one call in microseconds. */
  share: (t: number) => number;
  meanUs: number;
  maxUs: number;
}
const wave = (t: number, k: number) => Math.sin(t * 0.7 + k) * 0.012 + Math.cos(t * 1.3 + k) * 0.008;
const MODEL: Model[] = [
  { index: 0, kind: 'module', share: (t) => 0.184 + wave(t, 0), meanUs: 148.4, maxUs: 210 },
  { index: 1, kind: 'module', share: (t) => (t < FX_AT ? 0 : 0.061 + wave(t, 1) / 2), meanUs: 49.2, maxUs: 388 },
  { index: 2, kind: 'module', share: () => 0.004, meanUs: 3.1, maxUs: 6 },
  { index: 0, kind: 'phase', share: () => 0.021, meanUs: 17, maxUs: 31 },
  { index: 1, kind: 'phase', share: () => 0.013, meanUs: 10.4, maxUs: 22 },
  { index: 2, kind: 'phase', share: () => 0.002, meanUs: 1.6, maxUs: 4 },
  { index: 0, kind: 'poll', share: () => 0.006, meanUs: 4.8, maxUs: 61 },
  { index: 1, kind: 'poll', share: () => 0.003, meanUs: 2.2, maxUs: 9 },
];
const MODULES = (t: number): ModuleRef[] => [
  { index: 0, name: 'test/saw', kind: 'machine' },
  ...(t >= FX_AT ? [{ index: 1, name: 'test/verb', kind: 'effect' as const }] : []),
  { index: 2, name: 'test/mon', kind: 'tool' },
];

export function createRecorder(): Recorder {
  let running = state === 'rec';
  let t = 0; // seconds recorded
  let last = Date.now();
  let elapsed = 0n;
  const sums = MODEL.map(() => 0n);
  const calls = MODEL.map(() => 0);
  const advance = () => {
    const now = Date.now();
    const dt = running ? Math.min(2, (now - last) / 1000) : 0;
    last = now;
    if (dt <= 0) return;
    t += dt;
    elapsed += BigInt(Math.round(dt * HZ));
    MODEL.forEach((m, i) => {
      const s = m.share(t);
      sums[i] += BigInt(Math.round(s * dt * HZ));
      calls[i] += s > 0 ? Math.round((s * dt) / (m.meanUs * 1e-6)) : 0;
    });
  };
  const snap = (): Snapshot => ({
    running,
    hz: HZ,
    elapsedTicks: elapsed,
    spans: MODEL.map((m, i) => ({
      row: i,
      kind: m.kind,
      index: m.index,
      calls: calls[i]!,
      sumTicks: sums[i]!,
      maxTicks: calls[i]! > 0 ? Math.round((m.maxUs * HZ) / 1e6) : 0,
    })),
    polls: [
      { owner: 'runtime', fn: 0x40101000, name: 'usb_bulk_tick' },
      { owner: 2, fn: 0x47e20040 },
    ],
  });
  return {
    async read(): Promise<ProfileReading> {
      await new Promise((r) => setTimeout(r, 60));
      const port = `${dev}-${dev === 'dn2' ? '1.12' : '1.17'}`;
      if (state === 'nodebug') return { noDebug: true, port: 'dt2-1.17', emulator };
      advance();
      const modules = MODULES(t);
      const r: ProfileReading = { noDebug: false, port, emulator, snap: snap(), modules };
      if (emulator && elapsed > 0n) {
        r.dsp = dspView(
          {
            blocks: 1000,
            series: [
              {
                series: 'pc',
                cpu: 'sharc',
                owners: [
                  { owner: 'test/saw', per_block: 31_600 },
                  ...(t >= FX_AT ? [{ owner: 'test/verb', per_block: 12_100 }] : []),
                  { owner: 'firmware', per_block: 51_000 },
                ],
                functions: [
                  { name: 'render · loop +0x40..+0x9c', module: 'test/saw', per_block: 24_648 },
                  { name: 'render · setup', module: 'test/saw', per_block: 4_740 },
                  { name: 'note_on', module: 'test/saw', per_block: 2_212 },
                  { name: 'send', module: 'test/verb', per_block: 12_100 },
                ],
              },
            ],
          },
          modules,
          dev === 'dn2' ? 92_796 : 194_244,
        );
      } else if (!emulator && elapsed > 0n) r.deviceDsp = { avgPct: 21.3, maxPct: 34 };
      return r;
    },
    async start() {
      advance();
      running = true;
      last = Date.now();
    },
    async stop() {
      advance();
      running = false;
    },
    async reset() {
      advance();
      t = 0;
      elapsed = 0n;
      sums.fill(0n);
      calls.fill(0);
    },
    async exportFile() {
      return { text: '{}\n', name: 'preview.dkprof.json' };
    },
  };
}
