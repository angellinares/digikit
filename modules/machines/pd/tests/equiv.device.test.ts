import { defineEquiv, DN2_TRIG, DT2_TRIG, UNINIT, type Equiv } from '@digi/regression/runtime/equiv.ts';

const SCENARIOS: Equiv[] = [
  {
    name: 'equiv-dyn-pd',
    device: 'dt2',
    modules: ['fadedlimes/pd'],
    play: { module: 'fadedlimes/pd', machine: 'pd' },
    hint: 7,
    trigFrame: DT2_TRIG,
    voices: 1,
    ignoreRunMachines: ['Phase Dist'],
    envRun: UNINIT,
    // onset 258, 24000 samples (DT2 1.17, note 60); approved by Em 2026-11-01 (exact RECIPS, finding 102)
    pcmSha256: 'eed2bf269be58b6bac0ba329007f1221c2f0920fd46eac60aa43d300cedb97e0',
  },
  {
    name: 'equiv-dyn-pd',
    device: 'dn2',
    modules: ['fadedlimes/pd'],
    play: { module: 'fadedlimes/pd', machine: 'pd' },
    hint: 4,
    hintRun: 5,
    trigFrame: DN2_TRIG,
    voices: 1,
    ignoreRunMachines: ['Phase Dist'],
    envRun: UNINIT,
    // onset 272, 24000 samples (DN2 1.12, note 60); approved by Em 2026-11-01 (exact RECIPS, finding 102)
    pcmSha256: 'a8143de5a6e1235c2fd1cfb2389a8cc05dd3824d0551d01b63f57927671d485c',
  },
];

defineEquiv(SCENARIOS);
