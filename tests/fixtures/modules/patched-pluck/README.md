# Pluck, patched in (test)

A patched-in plucked-string machine: its own copy of the DSP source (`src/dsp/pluck.s`, no file outside this folder), applied to a firmware image by patchkit. The Digitakt II machine has the permanent type 9; on the Digitone II it is the `own_dsp` machine of type 9 with its SYN page.
It keeps the patched-in path under test: the equiv test of the pluck module (`modules/machines/pluck/tests/equiv.device.test.ts`) uses it as the twin of the run-time machine (PCM and rows must be equal), and the panel tests in `tests/`, the smoke matrix and the `pk_proof_pluck*`, `pk_proof_voice*` and `pk_tiers` tools patch it into an image.
Needs `core/dsp-voice`, `core/machines` and `core/params`, which patchkit adds. Emulator only. It is not in the public module list of the browser page.
Its DSP copy (`src/dsp/pluck.s`) and `tests/pluck_ref.py` must follow the pluck module's DSP when its sound changes on purpose: that test compares the two.
