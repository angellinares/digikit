# HANG-D (test)

A run-time machine (Digitakt II) of the SDK contract whose `render` enters an endless loop on the third block of a note. It exists to show
what the debug log reports when the SHARC stops inside a machine routine: `DSP_STOPPED` with the last breadcrumb (step, voice, machine type),
the ABOUT row `DSP STOPPED`. Pack it with `patchkit pack test/dyn-dsp-hang --device dt2 -o hang-d.dkm`. It is the negative fixture of the machine
conformance suite (`NEGATIVE` in `packages/sharc-rig-kit/src/spec.ts`; finding 48). Do not select it on a real device: the DSP stays stopped until the restart.
