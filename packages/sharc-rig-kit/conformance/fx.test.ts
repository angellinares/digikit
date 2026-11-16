/**
 * Every effect module must pass this suite, whatever it does (docs/TESTING.md "FX conformance").
 *
 * The cases come from src/cases.ts `fxCases`: test signals (silence, DC, square, noise, impulse, sine, ramp), the parameter words at their limits, per-block
 * modulation, an effect that is switched on and off, and a long run. Each runs the effect behind core/fx on the generated native core (src/fx.ts) and checks,
 * for every block, that the hook returns, the context and the stock function's registers come back, the adapter did not have to refuse the block, the module
 * wrote only memory it owns, the master stays bounded and the instruction count is within the budget (src/checks.ts).
 *
 * One run per module with `DIGI_JOBS` at a time. Select with DIGI_CONFORM_MODULES=test/dyn-fx-gain,... (`mise run module:check NAME` sets it). Reports go to
 * `DIGI_CONFORM_REPORTS`, else dist/conformance-rig/ of the worktree (never committed).
 */
import path from 'node:path';
import { beforeAll, describe, expect, it } from 'vitest';
import {
  FX_ADAPTER_MAX,
  FX_NEGATIVE,
  FxRig,
  ROOT,
  conformFx,
  fxLabel,
  fxSpecs,
  jobsFromEnv,
  runPool,
  writeReport,
  type FxReport,
} from '../src/index.ts';

const GROUPS = ['signals', 'params', 'modulation', 'switching', 'long'];
const MAX_SHOWN = 6;

const wanted = new Set((process.env.DIGI_CONFORM_MODULES ?? '').split(',').filter(Boolean));
const specs = fxSpecs().filter((s) => !wanted.size || wanted.has(s.module));
const reports = new Map<string, FxReport>();

beforeAll(async () => {
  const done = await runPool(specs, jobsFromEnv(), (s) => conformFx(s));
  specs.forEach((s, i) => {
    reports.set(fxLabel(s), done[i]!);
    writeReport(done[i]!);
  });
}, 7_200_000);

if (specs.length > 0) {
  describe.each(GROUPS)('effect conformance, group %s', (group) => {
    it.each(specs.map(fxLabel))('%s', (label) => {
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
    it.each(specs.map(fxLabel))('%s stays within the budget', (label) => {
      const r = reports.get(label)!;
      const steady = Math.max(...r.cases.map((c) => c.steady));
      console.log(
        `${label}: worst block ${steady} instructions (budget ${r.budget.effect} for the effect + ${FX_ADAPTER_MAX} for core/fx)`,
      );
      expect(steady).toBeLessThanOrEqual(r.budget.effect + r.budget.adapter);
    });
  });
}

it('finds effect modules', () => {
  expect(specs.length > 0 || wanted.size > 0, 'no effect module found').toBe(true);
});

const negatives = fxSpecs(true).filter((s) => !wanted.size || wanted.has(s.module));
if (negatives.length > 0)
  describe('the suite rejects an effect that writes a NaN', () => {
    it.each(negatives.map((s) => [fxLabel(s), s] as const))('%s', async (_label, spec) => {
      const rep = await conformFx(spec, { groups: ['signals'] });
      const failures = rep.cases.flatMap((c) => (c.failure ? [c.failure] : []));
      expect(failures.length).toBeGreaterThan(0);
      for (const f of failures) expect(f).toContain(FX_NEGATIVE[spec.module]);
    });
  });

describe('the write-set check', () => {
  // tests/support/fx_effects.s: `_te_stray` stores into the master block, which is read only for an effect; `_te_half` stores only its output
  const sources = [path.join(ROOT, 'tests/support/fx_effects.s')];
  const found = async (pc: string) => {
    const rig = await FxRig.open([{ pc, state: '_te_state_a' }], { sources, recordStores: true });
    try {
      return rig.foreignStores(16, (await rig.block(Array(64).fill(0.25))).stores);
    } finally {
      await rig.close();
    }
  };

  it('finds a store into the input block', async () => {
    const stray = await found('_te_stray');
    expect(stray.length).toBeGreaterThan(0);
    expect(stray[0]).toContain('master block');
  });

  it('finds nothing in a well-behaved effect', async () => {
    expect(await found('_te_half')).toEqual([]);
  });
});
