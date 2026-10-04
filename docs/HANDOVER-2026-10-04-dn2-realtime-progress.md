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

State: desktop app about 1.22x real time with PGO and three fast regions.
The fast tier covers most of the hot set, but its fixed per-call cost makes
small regions slower than the generated code, so wider coverage does not pay
yet.

1. Cut the per-call cost: profile the glue (register copies, window
   resolution, flag replay) and let kernels read and write `St` directly by
   offset (works in WebAssembly too: `St` lives in linear memory).
2. Measure the fast tier as the only compiled tier (interpreter plus fast
   tier, no generated cache) with run-time hot-region selection. That is what
   the browser will run, and it removes the 4-minute builds.
3. Grow regions across CALL/RTS and chains, so each call does more work.
4. ColdFire becomes the limit near 0.83x real time (its step window was 632 ms
   of the 0.759 s workload); profile it once the DSP is below real time.
5. WebAssembly backend; firmware-free inputs (loader-stream image, cold start
   checked against the ready state).
