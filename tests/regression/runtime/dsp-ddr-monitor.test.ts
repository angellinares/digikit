import { createHash, randomUUID } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { repoRoot, resolveInputs } from '@digi/test-support';
import { describe, expect, test } from 'vitest';
import { withSession } from '../support.ts';

const binary = process.env.DIGI_DDR_MONITOR_EMUD;
const enabled = Boolean(binary);
const root = repoRoot();
const hash = (file: string) => createHash('sha256').update(fs.readFileSync(file)).digest('hex');
type Monitor = {
  active_core_profile: string;
  dsp_icount: number;
  armed: boolean;
  range: { start: number; end: number } | null;
  first: { address: number; width: number; kind: string } | null;
};

async function bounded<T>(operation: Promise<T>, ms: number, label: string): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      operation,
      new Promise<never>((_, reject) => {
        timer = setTimeout(() => reject(new Error(`${label} exceeded ${ms} ms`)), ms);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

// Explicit existing daemon and registry-ready inputs only; never build or boot
// a new fixture on import. Each session has one active coupled DSP core.
for (const device of ['dn2', 'dt2'] as const) {
  const firmware = device === 'dn2' ? '1.12' : '1.17';
  describe.skipIf(!enabled)(`${device} ${firmware} bounded DDR diagnostic`, () => {
    const runDir = path.join(root, 'out/runs/ddr-monitor-rpc/acceptance', device, randomUUID());
    if (enabled) {
      if (!path.isAbsolute(binary!) || !fs.existsSync(binary!))
        throw new Error('DIGI_DDR_MONITOR_EMUD must name an existing absolute daemon path');
      fs.mkdirSync(runDir, { recursive: true });
    }
    const handle = withSession({ runDir, binary, device, firmware, dsp: 'sharc', restart: false });

    test('orders lifecycle commands and records helper-path and bounded stock observations', async () => {
      const session = handle.session;
      const evidence: Record<string, unknown> = {};
      let stopped = false;
      const rpc = async <T>(method: string, params: object = {}): Promise<T> => {
        if (stopped) throw new Error('DDR monitor proof has stopped');
        return bounded(session.client.call<T>(method, params), 5000, method);
      };
      const monitor = (params: object = {}) => rpc<Monitor>('dsp.ddr_monitor', params);
      const run = async () => {
        const inputs = resolveInputs(device, firmware);
        evidence.identity = {
          binary: { path: binary, realpath: fs.realpathSync(binary!), sha256: hash(binary!) },
          inputs,
          hashes: { syx: hash(inputs.syx), dspImage: hash(inputs.dspImage), dspState: hash(inputs.dspState) },
          load: session.loadResult,
        };
        await rpc('run.mode', { mode: 'paused' });
        const initial = await monitor({ action: 'disarm' });
        expect(initial.armed).toBe(false);
        expect(initial.range).toBeNull();
        expect(initial.first).toBeNull();
        const report = await rpc<{ audio: { dsp_core: string } }>('audio.report');
        expect(initial.active_core_profile).toBe(report.audio.dsp_core);
        expect(initial.active_core_profile.length).toBeGreaterThan(0);
        // The core profile is a build label, not a firmware-version assertion.
        evidence.activeCoreProfile = initial.active_core_profile;
        const address = 0x80000000;
        const before = await rpc<{ addr: number; value: number | null }>('dsp.peek_word', { addr: address });
        const full = await monitor({ action: 'arm', start: address, end: 0xa0000000 });
        expect(full.range).toEqual({ start: address, end: 0xa0000000 });
        expect(full.first).toBeNull();
        const range = { start: address, end: address + 4 };
        const armed = await monitor({ action: 'arm', ...range });
        expect(armed.armed).toBe(true);
        expect(armed.range).toEqual(range);
        expect((await monitor()).first).toBeNull(); // Status never self-latches.
        const after = await rpc<typeof before>('dsp.peek_word', { addr: address });
        expect({ addr: after.addr, value: after.value }).toEqual({ addr: before.addr, value: before.value });
        const hit = await monitor();
        expect(hit.first).toEqual({ address, width: 4, kind: 'cpu_read' });
        evidence.helperObservation = {
          before,
          after,
          hit,
          meaning:
            'Host peek deliberately traverses the CPU read helper; this is not guest-origin proof. Unknown data still counts as an attempted read.',
        };
        for (const params of [
          { action: 'arm', start: address, end: address },
          { action: 'arm', start: 0x7fffffff, end: address + 4 },
          { action: 'arm', start: address, end: 0xa0000001 },
          { action: 'arm', start: address, end: 0x1_0000_0000 },
          { action: 'unknown' },
        ]) {
          await expect(monitor(params)).rejects.toMatchObject({ code: -32602 });
          const preserved = await monitor();
          expect(preserved.range).toEqual(range);
          expect(preserved.first).toEqual(hit.first);
        }
        const cleared = await monitor({ action: 'clear' });
        expect(cleared.armed).toBe(true);
        expect(cleared.range).toEqual(range);
        expect(cleared.first).toBeNull();
        expect((await monitor()).first).toBeNull();
        await rpc('dsp.peek_word', { addr: address });
        expect((await monitor()).first).toEqual(hit.first);
        const disarmed = await monitor({ action: 'disarm' });
        expect(disarmed.armed).toBe(false);
        expect(disarmed.range).toBeNull();
        expect(disarmed.first).toBeNull();

        // Separate finite stock execution over an arbitrary diagnostic interval.
        // Either a hit or no hit is evidence only for this bounded observation;
        // neither outcome selects an allocation or proves the interval is free.
        const stockRange = { start: 0x9fff0000, end: 0x9fff1000 };
        const stockStart = await monitor({ action: 'arm', ...stockRange });
        const status = await rpc<{ dsp_frames: number }>('status');
        await rpc('run.mode', { mode: 'free' });
        await expect
          .poll(async () => (await rpc<{ dsp_frames: number }>('status')).dsp_frames, { timeout: 10000, interval: 50 })
          .toBeGreaterThan(status.dsp_frames);
        await rpc('run.mode', { mode: 'paused' });
        const stockEnd = await monitor();
        expect(stockEnd.dsp_icount).toBeGreaterThan(stockStart.dsp_icount);
        expect(stockEnd.range).toEqual(stockRange);
        expect(stockEnd.active_core_profile).toBe(initial.active_core_profile);
        evidence.stockObservation = {
          start: stockStart,
          end: stockEnd,
          meaning: 'Bounded emulator observation; no ownership, capacity or hardware claim.',
        };
        await monitor({ action: 'disarm' });
        await bounded(session.expectNoFault('DDR monitor lifecycle'), 5000, 'no-fault check');
      };
      try {
        await bounded(run(), 30000, 'DDR monitor proof');
        fs.writeFileSync(path.join(runDir, 'result.json'), JSON.stringify({ passed: true, ...evidence }, null, 2));
      } catch (error) {
        fs.writeFileSync(
          path.join(runDir, 'result.json'),
          JSON.stringify({ passed: false, error: String(error), ...evidence }, null, 2),
        );
        throw error;
      } finally {
        stopped = true;
        await bounded(session.close(), 10000, 'owned daemon shutdown');
      }
    }, 45000);
  });
}
