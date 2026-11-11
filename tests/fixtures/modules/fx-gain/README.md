# FX gain (test)

A patched-in effect of the FX contract (`<dk/fx/fx.h>`) that scales the master block by 0.5. It is the fixture of `core/fx`: the conformance
suite and the emulator proof use it, and nothing else. Its gain word `_fg_gain` can be changed at run time with the emulator's `dsp.patch`.
