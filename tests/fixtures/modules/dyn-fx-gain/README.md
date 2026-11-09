# FX gain, run time (test)

An effect of the FX contract (`<dk/fx.h>`) that is loaded at run time (`.dkm`, DSP section version 4) on the Digitakt II and the Digitone II: it scales the master block by its
parameter word p0 (an IEEE float, 0.5 by default) and counts its calls in its state word. It is the fixture of the run-time effect scenario
(`modules/core/adapters/dynfx/tests/fx-chain.device.test.ts`). The patched-in effect of the same kind, for the adapter's tests, is `test/fx-gain`.
