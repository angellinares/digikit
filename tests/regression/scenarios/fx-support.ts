import { spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { afterAll, afterEach, beforeAll, expect, test } from 'vitest';
import { repoRoot } from '@digi/test-support';
import type { Scenario } from './dn2-trackfx.ts';

export interface Row {
  analysis: { basic: { nonfinite: number; dc: number[]; peak: number; growth: boolean } };
  decoded: Record<string, number>;
  pcm: {
    shape: number[];
    reference_shape: number[];
    reference_equal: boolean;
    reference_max_abs_diff: number | null;
    reference: string;
    mono_equal: boolean;
  };
  bits: Record<string, number | null>;
  tf: { fs_candidates: number[][]; [key: string]: unknown };
  sf: Record<string, number | null>;
  delay: { echo_onsets_s: number[] };
}

type Rendered = ReturnType<typeof fxRender>;

/** Register an oracle: `sweep` oracles run only with DIGI_FX_SWEEP=1; with `subsets`, DIGI_FX_NO_AOT_SUBSET=1 keeps only `subset` ones. */
export function fxOracles(needs: Map<string, string[]>, { subsets }: { subsets: boolean }) {
  const full = process.env.DIGI_FX_SWEEP === '1';
  const noAotSubset = process.env.DIGI_FX_NO_AOT_SUBSET === '1';
  return function oracle(
    identity: string,
    names: string[],
    body: () => void,
    { sweep = false, subset = false }: { sweep?: boolean; subset?: boolean } = {},
  ) {
    needs.set(identity, names);
    test.skipIf((sweep && !full) || (subsets && noAotSubset && !subset))(identity, body);
  };
}

/** The decoded 16-bit value of the parameter whose key starts with `prefix`. */
export function raw(rendered: Rendered, name: string, prefix: string): number {
  const value = Object.entries(rendered.row(name).decoded).find(([key]) => key.startsWith(`${prefix} `))?.[1];
  expect(value, `${name} ${prefix}`).toBeDefined();
  return value! & 0xffff;
}

/** Gates, finite, no DC, peak limit and (with `growth`) no level growth. */
export function expectHealthy(
  rendered: Rendered,
  name: string,
  { peak = 1, growth = true }: { peak?: number; growth?: boolean } = {},
) {
  const basic = rendered.row(name).analysis.basic;
  expect(rendered.gates(name)?.ok).toBe(true);
  expect(basic.nonfinite).toBe(0);
  expect(Math.max(...basic.dc.map(Math.abs))).toBeLessThan(1e-3);
  expect(basic.peak).toBeLessThanOrEqual(peak);
  if (growth) expect(basic.growth).toBe(false);
}

/** The UI values (high byte) of the parameters that differ from the `reference` scenario. */
export function changedUi(rendered: Rendered, name: string, reference: string) {
  const base = rendered.row(reference).decoded;
  return Object.fromEntries(
    Object.entries(rendered.row(name).decoded)
      .filter(([key, value]) => base[key] !== value)
      .map(([key, value]) => [key.split(' ')[0], (value & 0xffff) >> 8]),
  );
}

export function expectMono(rendered: Rendered, name: string) {
  expect(rendered.row(name).pcm.mono_equal).toBe(true);
}

export function expectZeroIsDry(rendered: Rendered, name: string) {
  const pcm = rendered.row(name).pcm;
  expect(pcm.shape).toEqual(pcm.reference_shape);
  expect(pcm.reference_equal).toBe(true);
}

/** Batch selected stock FX scenarios through the existing Python renderer/analysis CLI. */
export function fxRender(
  needs: Map<string, string[]>,
  { family, scenarios, device = 'dn2' }: { family: string; scenarios: Scenario[]; device?: 'dn2' | 'dt2' },
) {
  const root = repoRoot();
  const run = path.join(root, 'out', 'regression', family, `${Date.now()}-${process.pid}`);

  let rows: Record<string, Row>;
  let entries: Record<string, { gates: { ok: boolean } }>;
  let active: ReturnType<typeof spawn> | undefined;

  // Own the process group, so cancellation also stops the Python CLI's native children.
  async function command(args: string[], label: string): Promise<void> {
    const log = path.join(run, `${label}.log`);
    const fd = fs.openSync(log, 'w');
    const child = spawn('uv', ['run', 'python', ...args], { cwd: root, detached: true, stdio: ['ignore', fd, fd] });
    active = child;
    fs.closeSync(fd);
    const kill = () => {
      if (child.pid) {
        try {
          process.kill(-child.pid, 'SIGKILL');
        } catch (error) {
          if ((error as NodeJS.ErrnoException).code !== 'ESRCH') throw error;
        }
      }
    };
    const timer = setTimeout(kill, 1_800_000);
    process.once('SIGINT', kill);
    process.once('SIGTERM', kill);
    try {
      await new Promise<void>((resolve, reject) => {
        child.once('error', reject);
        child.once('exit', (code, signal) =>
          code === 0 ? resolve() : reject(new Error(`${label}: exit ${code ?? signal}; see ${log}`)),
        );
      });
    } finally {
      clearTimeout(timer);
      process.off('SIGINT', kill);
      process.off('SIGTERM', kill);
      kill();
      active = undefined;
    }
  }
  let failed = false;
  let ran = 0;
  afterEach((context) => {
    ran++;
    if (context.task.result?.state !== 'pass' && context.task.result?.state !== 'skip') failed = true;
  });
  afterAll(() => {
    if (active?.pid) {
      try {
        process.kill(-active.pid, 'SIGKILL');
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== 'ESRCH') throw error;
      }
    }
    if (process.env.DIGI_FX_KEEP_RUNS === '1') return;
    if (ran && !failed) {
      fs.rmSync(run, { recursive: true, force: true });
      return;
    }
    // Reports, decoded measurements and logs survive failures; PCM remains in the shared content cache.
    if (fs.existsSync(run))
      for (const name of fs.readdirSync(run, { recursive: true })) {
        if (String(name).endsWith('.wav')) fs.unlinkSync(path.join(run, String(name)));
      }
  });
  beforeAll(async ({}, suite) => {
    const selected = suite.tasks.filter((task) => task.mode === 'run').flatMap((task) => needs.get(task.name) ?? []);
    const names = new Set(selected);
    for (const name of selected) {
      const scenario = scenarios.find((s) => s.name === name)!;
      const note = scenario.events.filter((event) => 'button' in event && event.button === 25);
      const held = (note[1]!.at_instr - note[0]!.at_instr) / 132_000_000 > 1;
      const prefix = name.split('-')[0];
      names.add(name.includes('-flt') ? 'tf-dry-flt' : `${prefix}-dry-${held ? 'hold' : 'tap'}`);
      const fx = name.split('-')[1];
      if (prefix === 'sf' && ['del', 'rev', 'chr'].includes(fx!) && !name.endsWith('-ref')) names.add(`sf-${fx}-ref`);
      if (name.endsWith('-rt1')) names.add(name.slice(0, -4));
    }
    if (!names.size) return;
    fs.mkdirSync(run, { recursive: true });
    console.log(`${device.toUpperCase()} ${family} artifacts: ${run} (${names.size} scenarios)`);
    const image = path.join(
      root,
      'out',
      'sections',
      device === 'dt2' ? 'dt2-1.17' : 'dn2-1.12',
      'section_3_MAIN_OS.bin',
    );
    if (!fs.existsSync(image)) throw new Error(`Missing MAIN OS descriptor image: ${image}`);
    const scenarioFile = path.join(run, 'scenarios.json');
    fs.writeFileSync(scenarioFile, JSON.stringify({ scenarios }));
    const jobs = Number(process.env.DIGI_JOBS ?? 1);
    if (!Number.isSafeInteger(jobs) || jobs < 1) throw new Error('DIGI_JOBS must be a positive integer');
    await command(
      [
        '-m',
        'tools.emulator.coupled',
        'scenario',
        '--device',
        device,
        ...(device === 'dt2' ? ['--dt2-card'] : []),
        ...names,
        '--scenarios',
        scenarioFile,
        '--no-build',
        '--check-generated',
        '--jobs',
        String(jobs),
        '--out',
        path.join(run, 'report.json'),
        '--peek',
        device === 'dt2' ? 'snd=*0x8000470c+0x14:142' : 'snd=*0x800052a0+0x48:202',
        '--peek',
        device === 'dt2' ? 'fx=*0x80004708+0:142' : 'fx=*0x800052a4+0:202',
      ],
      'render',
    );
    await command(['-m', 'tools.emulator.fxcheck', run, '--device', device, '--no-plots'], 'analysis');
    rows = JSON.parse(fs.readFileSync(path.join(run, 'fxcheck.json'), 'utf8'));
    const report = JSON.parse(fs.readFileSync(path.join(run, 'report.json'), 'utf8')) as {
      scenarios: { name: string; gates: { ok: boolean } }[];
    };
    entries = Object.fromEntries(report.scenarios.map((entry) => [entry.name, entry]));
  }, 1_900_000);

  return {
    row(name: string): Row {
      const result = rows[name];
      if (!result) throw new Error(`Missing analysed scenario ${name}; see ${run}`);
      return result;
    },
    gates(name: string) {
      return entries[name]?.gates;
    },
  };
}
