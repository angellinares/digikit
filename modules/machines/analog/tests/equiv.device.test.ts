import { defineEquiv, DN2_TRIG, DT2_TRIG, UNINIT, type Equiv } from '@digi/regression/runtime/equiv.ts';

const SCENARIOS: Equiv[] = [
  {
    name: 'equiv-dyn-analog',
    device: 'dt2',
    modules: ['fadedlimes/analog'],
    play: { module: 'fadedlimes/analog', machine: 'analog' },
    hint: 7,
    trigFrame: DT2_TRIG,
    voices: 1,
    ignoreRunMachines: ['Analog'],
    envRun: UNINIT,
    // onset 256, 24000 samples (DT2 1.17, note 60); approved by Em 2026-11-01 (exact RECIPS, finding 102)
    pcmSha256: '427c8f33e23779303e4a2161e77c168f9b5e4833522fba23d901f3442ffeeb8a',
  },
  {
    name: 'equiv-dyn-analog',
    device: 'dn2',
    modules: ['fadedlimes/analog'],
    play: { module: 'fadedlimes/analog', machine: 'analog' },
    hint: 4,
    hintRun: 5,
    trigFrame: DN2_TRIG,
    voices: 1,
    ignoreRunMachines: ['Analog'],
    envRun: UNINIT,
    // onset 328, 24000 samples (DN2 1.12, note 60); approved by Em 2026-11-01 (exact RECIPS, finding 102)
    pcmSha256: '44db3540cf67de6e20ef5cda8e15f5a45bef5bbdc6c86213a7f6ea1dbf8f651e',
  },
];

defineEquiv(SCENARIOS);
