# FX NaN (test)

A negative fixture of the FX contract: its output holds a NaN. `core/fx` plays the block dry and counts a fault, and the FX conformance suite
(`packages/sharc-rig-kit/conformance/fx.test.ts`) must report this module as failing (`FX_NEGATIVE` in `spec.ts`).
