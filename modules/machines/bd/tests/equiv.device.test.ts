import { defineEquiv, DN2_TRIG, DT2_TRIG, type Equiv } from '@digi/regression/runtime/equiv.ts';

const SCENARIOS: Equiv[] = [
  {
    name: 'equiv-dyn-bd',
    device: 'dt2',
    modules: ['fadedlimes/bd'],
    play: { module: 'fadedlimes/bd', machine: 'bd' },
    hint: 7,
    trigFrame: DT2_TRIG,
    voices: 1,
    ignoreRunMachines: ['Bass Drum'],
    // onset 194, 24000 samples (DT2 1.17, note 60); approved by Em 2026-11-01 (exact RECIPS, finding 102)
    pcmSha256: '8de331fb3fec4648ef530b0f56e1fd8cf2634c094e7dd4631b472c2942f5dceb',
  },
  {
    name: 'equiv-dyn-bd',
    device: 'dn2',
    modules: ['fadedlimes/bd'],
    play: { module: 'fadedlimes/bd', machine: 'bd' },
    hint: 4,
    hintRun: 5,
    trigFrame: DN2_TRIG,
    voices: 1,
    ignoreRunMachines: ['Bass Drum'],
    // onset 270, 24000 samples (DN2 1.12, note 60); approved by Em 2026-11-01 (exact RECIPS, finding 102)
    pcmSha256: '5838f6e275cc21b45d6c97b43e1b48e1e6876517b18b07c527dae611b1325622',
  },
];

defineEquiv(SCENARIOS);
