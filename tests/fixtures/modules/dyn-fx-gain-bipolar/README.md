# FX gain bipolar, run time (test)

An effect of the FX contract (`<dk/fx/fx.h>`) with a signed knob, loaded at run time on the Digitakt II and the Digitone II. BIAS runs from -64 to +63 and starts at 0;
the routine reads it as the float p0 and scales the master block by `1 + p0 / 64` (0 is transparent, -64 mutes, +63 is about 1.98). It counts its calls in its state word.
It is the fixture of the signed value template of core/fxpages (`modules/core/adapters/fxpages/tests/fx-bipolar.device.test.ts`), in place of the adapter legs of the frequency shifter test. The default route is the send; it may be switched to the master.
