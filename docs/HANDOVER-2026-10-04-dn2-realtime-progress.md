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

The direction is a load-time translator (the fast tier) with Cranelift for
native and a WebAssembly backend for the browser, so that a GitHub Pages
build carries no firmware-derived code and runs any uploaded or patched
.syx. The AOT generated cache stays a local development path.

1. Widen the fast tier to the top 19 regions (about 50% of DSP time):
   multifunction computes, the 4a/15b/4b/15a memory forms, conditions and
   flag reads inside bodies, nested and in-region DO loops, non-loop regions
   with jumps and calls, rare ALU ops. Each family is a separate lowering
   file with its own differential test, so the work runs in parallel.
2. Pick hot regions at run time from entry counts instead of
   `SHARC_FAST_REGIONS`.
3. Replace chain capping: let generated blocks end at fast-tier entries.
4. WebAssembly backend for the same kernels (`native/sharc-jit` already
   splits modules that share memory and a function table).
5. Firmware-free inputs: Rust loader-stream image parser, DSP cold start
   compared with the ready state, idle addresses per firmware, then a
   .syx-only `digi_load_coupled`.
6. Live check on the desktop app after each step.
