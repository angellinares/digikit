import { defineEquiv, DT2_TRIG, type Equiv } from '@digi/regression/runtime/equiv.ts';

const SCENARIOS: Equiv[] = [
  {
    // no twin and no PCM pin: the rows equal a boot of no module and the note of test/sine is audible
    name: 'equiv-dt2-sine',
    device: 'dt2',
    modules: ['test/sine'],
    play: { module: 'test/sine', machine: 'sine' },
    trigFrame: DT2_TRIG,
    voices: 1,
    ignoreRunMachines: ['Sine'],
  },
  {
    name: 'equiv-dn2-set',
    device: 'dn2',
    modules: ['test/dyn-dn2', 'test/sine'],
    twin: 'test/twin-dn2',
    hintRun: 6,
    trigFrame: 7000,
    voices: 2,
    ignoreRunMachines: ['Sine'],
    // onset 400, 24000 samples (DN2 1.12, note 60, 261.65 Hz); approved by Em 2026-11-01 (exact RECIPS, finding 102)
    pcmSha256: '330e9725263aa2aab1da1c4f9b562b5b0d64c9896212106c4eddc3c4547db1d3',
  },
];

defineEquiv(SCENARIOS);
