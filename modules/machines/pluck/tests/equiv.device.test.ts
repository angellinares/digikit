import { defineEquiv, DN2_TRIG, type Equiv } from '@digi/regression/runtime/equiv.ts';

const SCENARIOS: Equiv[] = [
  {
    // the note starts shortly after the panel setup ends (frame 3330 in both rigs)
    name: 'equiv-dyn-pluck',
    device: 'dt2',
    modules: ['digikit/pluck'],
    twin: 'test/patched-pluck',
    play: { module: 'digikit/pluck', machine: 'pluck' },
    staticType: 9,
    hint: 7,
    trigFrame: 3500,
    voices: 1,
    ignoreRunMachines: [],
  },
  {
    // the DN2 list has the patched-in pluck at position 4, the run-time machines follow MIDI
    name: 'equiv-dyn-pluck',
    device: 'dn2',
    modules: ['digikit/pluck'],
    twin: 'test/patched-pluck',
    refPlain: true,
    play: { module: 'digikit/pluck', machine: 'pluck' },
    hint: 4,
    hintRun: 5,
    trigFrame: DN2_TRIG,
    voices: 1,
    ignoreRunMachines: [],
  },
  {
    // Full tier: the same twin comparison with the note later in the run
    name: 'equiv-mixed',
    device: 'dt2',
    full: true,
    modules: ['digikit/pluck'],
    twin: 'test/patched-pluck',
    play: { module: 'digikit/pluck', machine: 'pluck' },
    staticType: 9,
    hint: 7,
    trigFrame: 9500,
    voices: 1,
    ignoreRunMachines: [],
  },
];

defineEquiv(SCENARIOS);
