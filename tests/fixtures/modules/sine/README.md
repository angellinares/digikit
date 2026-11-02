# SINE (test)

A pitched sine machine of the SDK contract with one level parameter (`sine_level`), loaded at run time on both devices (one DSP body). It is the template of a machine module and the sound source of the run-time tests (`tests/regression/runtime/equiv.test.ts`, the pitch tests of `tests/patchkit/test_sine.py`). Pack it with `patchkit pack test/sine --device dt2 -o sine.dkm` (`--device dn2` for the Digitone II).
