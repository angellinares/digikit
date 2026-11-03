import { defineMemaudit, type AuditSet } from '@digi/regression/runtime/memaudit-suite.ts';

/**
 * The ColdFire memory audit (Python scenarios memaudit-{dt2,dn2} and memaudit-{dt2,dn2}-stray, pk_digikit_memaudit.py; mapping in docs/plans/ledgers/debug-card-safe.md).
 * test/dyn-demo's slot function runs on every UI pass; test/c-demo is loaded and not called.
 */

/** The clean sets: the guard must report no store; a run-time machine of the set is on track 1. */
defineMemaudit((device): AuditSet =>
  device === 'dt2'
    ? {
        title: 'clean set',
        modules: ['test/c-demo', 'test/dyn-demo', 'test/sine'],
        mustRun: ['test/dyn-demo'],
        machines: [{ track: 1, module: 'test/sine', machine: 'sine', hint: 7 }],
      }
    : {
        title: 'clean set',
        modules: ['test/c-demo', 'test/dyn-demo', 'test/dyn-dn2'],
        mustRun: ['test/dyn-demo'],
        machines: [{ track: 1, module: 'test/dyn-dn2', machine: 'dyn_pluck', hint: 5 }],
      },
);

/** Negative control: test/dyn-demo-stray stores one word 128 KiB above its counter, in the arena and outside its block, on its first slot call. */
defineMemaudit(() => ({
  title: 'stray store',
  modules: ['test/c-demo', 'test/dyn-demo-stray'],
  mustRun: ['test/dyn-demo-stray'],
  stray: 'test/dyn-demo-stray',
}));
