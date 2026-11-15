import { mkdtempSync, readFileSync, realpathSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { afterAll, describe, expect, it } from 'vitest';
import { reportName, reportsDir, worktreeRoot, worstSteady, writeReport, type MachineReport } from '../src/report.ts';
import { ROOT } from '../src/rig.ts';

const dirs: string[] = [];
afterAll(() => {
  for (const d of dirs) rmSync(d, { recursive: true });
});

const report: MachineReport = {
  label: 'a/machine@dt2',
  runner: 'sharc-rig-kit',
  build_seconds: 1.5,
  budget: { voice_block: 12140, note_on_block: 194244, headroom: 194244 },
  params: ['a'],
  cases: [
    { group: 'limits', name: 'x', blocks: 4, steady: 100, noteon: 900, failure: null, seconds: 0.5 },
    { group: 'long', name: 'y', blocks: 2000, steady: 5546, noteon: 800, failure: null, seconds: 2 },
  ],
};

describe('reports', () => {
  it('are named after the module with / as __ and the device or fx after @', () => {
    expect(reportName('a/machine@dt2')).toBe('a__machine@dt2.json');
    expect(reportName('a/effect@fx')).toBe('a__effect@fx.json');
  });

  it('are written as JSON that module:check reads: the cases hold the steady block of each', () => {
    const dir = mkdtempSync(path.join(tmpdir(), 'rig-report-'));
    dirs.push(dir);
    const file = writeReport(report, path.join(dir, 'deeper'));
    expect(file).toBe(path.join(dir, 'deeper', 'a__machine@dt2.json'));
    const text = readFileSync(file, 'utf8');
    expect(JSON.parse(text)).toEqual({ ...report, worktree: worktreeRoot() });
    expect(text).toContain('\n "label": "a/machine@dt2"'); // one space of indent, as the Python workers wrote it
  });

  it('give the DSP cost of a module as the worst steady block of any case', () => {
    expect(worstSteady(report)).toBe(5546);
    expect(worstSteady({ cases: [] })).toBe(0);
  });

  it('record the worktree that wrote them, and default to its own dist/, never the shared out/', () => {
    const prev = process.env.DIGI_CONFORM_REPORTS;
    delete process.env.DIGI_CONFORM_REPORTS;
    try {
      expect(reportsDir()).toBe(path.join(ROOT, 'dist/conformance-rig'));
    } finally {
      if (prev !== undefined) process.env.DIGI_CONFORM_REPORTS = prev;
    }
    const dir = mkdtempSync(path.join(tmpdir(), 'rig-report-'));
    dirs.push(dir);
    const written = JSON.parse(readFileSync(writeReport(report, dir), 'utf8'));
    expect(written.worktree).toBe(realpathSync(ROOT));
  });
});
