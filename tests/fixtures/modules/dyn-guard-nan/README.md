# NAN-D (test)

A run-time machine (Digitakt II and Digitone II; the DSP source is one contract body for both) whose `render` fills its output block with NaN while the voice is active. It exists to show the runtime
guard of `core/dsp-voice`: the block is zeroed, the voice ends, the fault is counted (`MACHINE_FAULT` in the digikit log) and, after eight faults
within a second, the machine is switched off (`MACHINE_DISABLED`). Pack it with `patchkit pack test/dyn-guard-nan --device dt2 -o nan-d.dkm` (`--device dn2` for the Digitone II: an `own_dsp` machine with a one-knob SYN page).
Emulator scenario: `debug-guard`. `clamp.s` is a rig fixture of `modules/core/adapters/dsp-voice/tests/test_dsp_guard_contract.py`; `nan_legacy.s`, `loud.s` and `clean.s` are rig fixtures of `modules/core/adapters/dsp-voice/tests/test_dsp_guard.py` (the raw form of a voice row).
