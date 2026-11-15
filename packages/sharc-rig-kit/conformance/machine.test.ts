/**
 * Every machine module must pass this suite, whatever it renders (docs/TESTING.md "Machine conformance").
 *
 * The cases come from the module's manifest (src/cases.ts): every parameter at its limits, parameters x notes, per-block modulation, long notes and retriggers.
 * Each case runs the module's DSP routine on the generated native core through core/dsp-voice (src/machine.ts) and checks, for every block, that the call
 * returns, the context registers come back, the output is finite and bounded, the module wrote only memory it owns and the instruction count is within the
 * budget (src/checks.ts). A module's own tests stay with the module.
 *
 * One run per (module, device) with `DIGI_JOBS` at a time; the tests below read its report, one test per case group. Select with
 * DIGI_CONFORM_MODULES=<author/name>,... and DIGI_CONFORM_DEVICE=dt2|dn2 (`mise run module:check NAME` sets them). Reports go to
 * `DIGI_CONFORM_REPORTS`, else dist/conformance-rig/ of the worktree (never committed).
 */
import { beforeAll, describe, expect, it } from 'vitest';
import {
  NEGATIVE,
  conformMachine,
  jobsFromEnv,
  machineKey,
  machineLabel,
  machineSpecs,
  runPool,
  writeReport,
  type MachineReport,
  type MachineSpec,
} from '../src/index.ts';

const GROUPS = ['limits', 'corners', 'modulation', 'long', 'retrigger', 'writes'];
const MAX_SHOWN = 6;

const wanted = new Set((process.env.DIGI_CONFORM_MODULES ?? '').split(',').filter(Boolean));
const device = process.env.DIGI_CONFORM_DEVICE ?? '';
const specs = machineSpecs().filter((s) => (!wanted.size || wanted.has(s.module)) && (!device || s.device === device));
const reports = new Map<string, MachineReport>();

beforeAll(async () => {
  // modules built from the same sources and parameters run once
  const first = new Map<string, MachineSpec>();
  for (const s of [...specs].reverse()) first.set(machineKey(s), s);
  const keys = [...first.keys()];
  const done = await runPool(keys, jobsFromEnv(), (key) => conformMachine(first.get(key)!));
  const byKey = new Map(keys.map((k, i) => [k, done[i]!]));
  for (const s of specs) {
    const report = { ...byKey.get(machineKey(s))!, label: machineLabel(s) };
    reports.set(report.label, report);
    writeReport(report);
  }
}, 7_200_000);

if (specs.length > 0) {
  describe.each(GROUPS)('machine conformance, group %s', (group) => {
    it.each(specs.map(machineLabel))('%s', (label) => {
      const cases = reports.get(label)!.cases.filter((c) => c.group === group);
      expect(cases.length, `${label}: no cases in group ${group}`).toBeGreaterThan(0);
      const bad = cases.filter((c) => c.failure);
      if (bad.length) {
        const shown = bad
          .slice(0, MAX_SHOWN)
          .map((c) => `  ${c.failure}`)
          .join('\n');
        const more = bad.length > MAX_SHOWN ? `\n  ... and ${bad.length - MAX_SHOWN} more` : '';
        expect.fail(`${label} group ${group}: ${bad.length} of ${cases.length} cases failed\n${shown}${more}`);
      }
    });
  });

  describe('the worst block', () => {
    it.each(specs.map(machineLabel))('%s stays within the budget', (label) => {
      const r = reports.get(label)!;
      const steady = Math.max(...r.cases.map((c) => c.steady));
      const noteon = Math.max(...r.cases.map((c) => c.noteon));
      console.log(
        `${label}: worst block ${steady} instructions (budget ${r.budget.voice_block}), worst note-on block ${noteon} (budget ${r.budget.note_on_block})`,
      );
      expect(steady).toBeLessThanOrEqual(r.budget.voice_block);
      expect(noteon).toBeLessThanOrEqual(r.budget.note_on_block);
    });
  });
}

it('finds machine modules', () => {
  expect(specs.length > 0 || wanted.size > 0 || device !== '', 'no machine module found').toBe(true);
});

const negatives = machineSpecs(true).filter((s) => !wanted.size || wanted.has(s.module));
if (negatives.length > 0)
  describe('the suite rejects a machine that breaks a rule', () => {
    it.each(negatives.map((s) => [machineLabel(s), s] as const))('%s', async (_label, spec) => {
      const rep = await conformMachine(spec, { groups: ['limits'] });
      const failures = rep.cases.flatMap((c) => (c.failure ? [c.failure] : []));
      expect(failures.length).toBeGreaterThan(0);
      for (const f of failures) expect(f).toContain(NEGATIVE[spec.module]);
    });
  });
