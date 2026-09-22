# Digitakt II / Digitone II — findings

Current targets are **Digitakt II OS 1.16** and **Digitone II OS 1.11**. Older
sections were written against DT2 1.15C and are kept for their history; where a
later section corrects one, it is marked **[C]**.

This file is the entry point into the lab notes for the Digitakt II / Digitone
II reverse-engineering work; the notes themselves live in `docs/findings/`,
one file per topic, in the chronological order they were written (later
sections often correct earlier ones — read a file top to bottom). Start with
the topic closest to what you're doing, or grep across all of them with
`rg . docs/findings/`.

Evidence classes used below:
**[V]** verified by running it here · **[D]** documented by prior research, not
re-checked · **[O]** open / nobody has established this.

## Contents

| file | covers |
| --- | --- |
| [`01-container-and-patching.md`](findings/01-container-and-patching.md) | Scope and the 2.01 firmwares, the ELE3 container format, integrity, packing, the version gate, recovery, what is patchable, the 1.16 rebuild, flash caves that survive a cold boot, cavefind |
| [`02-machines-and-parameters.md`](findings/02-machines-and-parameters.md) | The ColdFire machine dispatch, the descriptor and parameter tables, display names, the type-7/PLACEHOLDER clone work, the permission check, the SRC slot model, CFADE, XSLICE on 1.16 |
| [`03-ui-and-panel.md`](findings/03-ui-and-panel.md) | Panel chords, the UI queue, MACHINE SEL, timer-rate behaviour, the GUI/replay disagreements |
| [`04-coldfire-dsp-link.md`](findings/04-coldfire-dsp-link.md) | The periodic DSPI2 frame, the frame capture, the frame link on 1.16, the machine type reaching the SHARC, the mirror/SRC-parameter path |
| [`05-sharc-isa-and-decoding.md`](findings/05-sharc-isa-and-decoding.md) | The SHARC+ instruction tables from the public manuals, the run of Type-NN decode corrections, the selache cross-check, the Selache round trip, parcel order |
| [`06-sharc-engine-and-startup.md`](findings/06-sharc-engine-and-startup.md) | The bounded startup probe, the frontier sprint, the machine-type consumer, the audio engine ingredients, the SRC page read, the machine type as a change detector, where SRC words go, CFADE discarded |
| [`07-emulator.md`](findings/07-emulator.md) | Emulation, display, DSP bring-up, making it run, the performance work, correct-speed playback, the serial console, boot, speed measurements, the QEMU verdict, two task counters |
| [`08-hardware-and-ghidra.md`](findings/08-hardware-and-ghidra.md) | The MCF5441x identification, eDMA, the MMIO hook, Ghidra tooling, RTTI/code seeds, Version Tracking, functions Ghidra misses |
| [`09-runtime-state.md`](findings/09-runtime-state.md) | Live musical state in RAM: the pattern and kit working-set tables |
| [`10-sharc-indirect-target-dossier.md`](findings/10-sharc-indirect-target-dossier.md) | Static dossier for the `FUN_1c642a` Type9b indirect target and provisional hook ranking |
| [`functions/README.md`](findings/functions/README.md) | Per-function SHARC notes, including Phase B wrapper and conditional-resampling-path entries |
