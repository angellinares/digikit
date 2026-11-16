import { defineEquiv, DT2_TRIG, type Equiv } from '@digi/regression/runtime/equiv.ts';

const SCENARIOS: Equiv[] = [
  {
    // the one exact full-firmware audio check of the DT2 run-time machine path (onset, level, mixer): onset 194, 24000 samples (DT2 1.17, note 60, 261.67 Hz, crest 1.420, harmonics -119 dB, L = R)
    name: 'equiv-dt2-sine',
    device: 'dt2',
    modules: ['test/sine'],
    play: { module: 'test/sine', machine: 'sine' },
    trigFrame: DT2_TRIG,
    voices: 1,
    ignoreRunMachines: ['Sine'],
    pcmSha256: '891847714d9349092255cb462ece20792246a56fbf31f2bcd281aa90217da517',
  },
  {
    name: 'equiv-dn2-set',
    device: 'dn2',
    modules: ['test/dyn-dn2', 'test/sine'],
    hint: 6,
    trigFrame: 7000,
    voices: 2,
    ignoreRunMachines: ['DynSyn', 'Sine'],
    // onset 400, 24000 samples (DN2 1.12, note 60, 261.65 Hz); approved by Em 2026-11-01 (exact RECIPS, finding 102)
    pcmSha256: '330e9725263aa2aab1da1c4f9b562b5b0d64c9896212106c4eddc3c4547db1d3',
  },
];

defineEquiv(SCENARIOS);
