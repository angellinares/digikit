/**
 * The report of a conformance run, in the JSON shape the Python workers wrote (out/runs/conformance/<id>@<device>.json, `<id>@fx.json`):
 * `module:check` reads `cases[].steady` to write `dsp_cost_voice` and `dsp_cost_fx`.
 */
import { mkdirSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { ROOT } from './rig.ts';

export interface CaseResult {
  group: string;
  name: string;
  blocks: number;
  /** the worst block that is not a note-on block, in instructions */
  steady: number;
  noteon: number;
  failure: string | null;
  seconds: number;
}

export interface MachineReport {
  label: string;
  runner: string;
  build_seconds: number;
  budget: { voice_block: number; note_on_block: number; headroom: number };
  params: string[];
  cases: CaseResult[];
}

export interface FxReport {
  label: string;
  runner: string;
  build_seconds: number;
  budget: { effect: number; adapter: number };
  n_params: number;
  cases: CaseResult[];
}

export const RUNNER = 'sharc-rig-kit';

/** The directory of the reports: `DIGI_CONFORM_REPORTS`, else out/runs/conformance. */
export const reportsDir = (): string => process.env.DIGI_CONFORM_REPORTS || path.join(ROOT, 'out/runs/conformance');

/** The file name of a report: `<id with / as __>@<device or fx>.json`. */
export const reportName = (label: string): string => `${label.replaceAll('/', '__')}.json`;

/** Write a report like Python's `json.dumps(report, indent=1)`. */
export function writeReport(report: MachineReport | FxReport, dir = reportsDir()): string {
  mkdirSync(dir, { recursive: true });
  const file = path.join(dir, reportName(report.label));
  writeFileSync(file, JSON.stringify(report, null, 1));
  return file;
}

/** The worst steady block over every case (the DSP cost a module records), or 0 when there is none. */
export const worstSteady = (report: { cases: CaseResult[] }): number =>
  Math.max(0, ...report.cases.map((c) => c.steady));

/** Round to two decimals, as the workers did for `seconds`. */
export const round2 = (x: number): number => Math.round(x * 100) / 100;
