# DN2 real-time audio — handover

Date: 2026-10-04. Branch `work/sharc-emulator`, HEAD `eecf7b8` plus
uncommitted work below. Results are in `docs/findings/07-emulator.md`
("app PGO, aligned period stop, gen provenance").

## State

The coupled DN2 workload takes about 1.075 s for 0.759 s of audio, about 1.42
times real time (it was 1.754 s). It needs about 30% less DSP time plus
headroom. DSP is the critical path.

Uncommitted, verified:
- `tools/native_pgo.py` (`train`, `check`, `plan`, `bench`, `bench-bins`) and
  the PGO path in `tools/native_emu.sh`. The current profile matches the tree:
  `out/native/pgo/dn2/aarch64-apple-darwin/profiles/a8c0e85c….profdata`.
  The next coupled `mise run emu` builds the app with it (first build about
  4 minutes).
- Aligned period stop: `native/sharc/src/lib.rs`, `native/boot/src/sharc_peer.rs`.
- `tools/sharc_entries_merge.py` (entries augmentation; not used by the
  accepted cache).
- `tests/test_lint.py` CLEAN additions.

Pre-existing dirty docs from earlier sessions are untouched.

## Known issues

- `tests/test_lint.py` format check fails on `tests/test_native_emu_launcher.py`
  (an over-long list). This was already the case before this work.
- `coupled_ready_exactness` in a debug (non-release) build overflows the
  `sharc-dsp` thread stack. Not checked at HEAD.
- `tests/test_sharc_transpile.py::test_native_compute_corpus_matches` fails on a
  stale `libsharc_native` dylib (`sharc_native_profile` not found). Rebuild the
  dylib before reading this as a regression.

## Archived evidence (ignored, under out/native/)

- `pgo-archive/20261003/`: the 2026-10-03 profile, accepted control and PGO
  test binaries, logs, and the first xctrace cost map with `profile/aggregate.py`.
- `pgo/dn2/aarch64-apple-darwin/`: profiles, manifest, bench runs,
  `profile-20261003b/` (three-run cost map, extended `aggregate.py`).
- `gen-provenance-20261003/`: generator inputs, command and `PROVENANCE.txt`.
- `fallback-map-20261003/`: interpreter fallback map, `report.py`.
- `codegen-audit-20261003/`: annotated AArch64 of the hottest regions.
- `codegen-fast-20261004/c1-c2-float-fastpaths.patch`: reverted float fast paths.

## Next steps

1. Live check: run `mise run emu` (PGO path), hold Trig 1, and record
   underrun counts against the 2026-10-03 run. Expect fewer underruns, but not
   zero.
2. Find the critical path in the hot region loops before more codegen work.
   Instruction-count savings did not translate to time. Measure cycles per
   iteration of `r_1C399A` with a microbenchmark of the generated function on
   a captured entry state, then test one change at a time (DM store undo-log
   avoidance with `_dm_write_nolog_b`, loop-stack model in locals, ASTAT/STKY
   mask specialisation).
3. Interpreter time (about 14%) now comes from model gates (irq_deferred,
   bank-switch gates, RTI and loop-register instructions), not missing
   entries. Handling those resume points in the runtime is a larger change.
4. Make `tools/sharc_rsgen.py` write its arguments and input hashes into the
   output directory.
