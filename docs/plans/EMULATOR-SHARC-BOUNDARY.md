# Emulator-to-SHARC Boundary Plan

## Goal

Use the ColdFire emulator as a bounded, reproducible boundary oracle to prove
how a per-track machine type leaves the ColdFire and enters the SHARC receive
path. The immediate success condition is a verified SHARC memory access whose
address is equivalent to:

```text
receive_buffer + 0x94 + 2 * track_index
```

The emulator does **not** execute the SHARC firmware. It can prove what the
ColdFire sends, when it sends it, and which ColdFire state produced it. Static
SHARC decoding and `tools/sharc_trace.py` must prove how the DSP consumes it.

Firmware and firmware-derived bytes stay under ignored `out/` paths and must
not be committed.

## Status

- [x] ColdFire frame handler can be entered directly from a snapshot.
- [x] `tools/sharcframe.py` captures the 2050-byte transmit frame.
- [x] Machine type is present at frame offset `0x94 + 2 * track`.
- [x] Digitakt II 1.15C and 1.16 section hashes match their source `.syx` files.
- [x] A bounded performance baseline has been measured.
- [x] A bounded DT2 1.16 track-0 direct-refresh experiment connects source
  `+0xa2`, SRAM row byte 0, and TX word `0x94` with active controls.
- [x] Exact DT2 1.16 panel replay reaches the relocated machine setter with
  `unblock=False`, `weakptr=False`, and clean/traced endpoint equivalence.
- [ ] Repeated experiments reuse one configured machine efficiently.
- [ ] Producer-side reads and writes have narrow, reproducible traces.
- [ ] Captured frames can be supplied as concrete input to the SHARC tracer.
- [ ] A SHARC load of the machine-type field has been verified.

## Fidelity rules

- [ ] Compare `sections/.source-sha256` or the selected
  `out/sections/*/.source-sha256` with `shasum -a 256 FIRMWARE.syx` before an
  emulator run.
- [ ] Bound every run by an expected endpoint and a wall timeout or by
  `--limit`.
- [ ] Use snapshots instead of replaying boot when the experiment permits it.
- [ ] Use `softfloat=True` and `bitmap=True` for state/frame experiments only.
  They preserve relevant state but change instruction counts.
- [ ] Do not use `fast=True`, `unblock=True`, or `weakptr=True` as evidence
  unless the finding explicitly accounts for their changed semantics.
- [ ] Keep precise/deterministic and exploratory performance results separate.
- [ ] Record verified firmware findings in `docs/FINDINGS.md`, not in this plan.
- [ ] Before marking a firmware finding **[V]**, have a second agent check it
  against the image bytes.

## Phase 0 — Freeze the baseline

**Purpose:** retain one reproducible command and output before changing the
emulator or capture tools.

- [x] Verify the Digitakt II 1.15C source hash against `sections/`.
- [x] Run three fresh `machinecommit` conditions from `boot400M.snap`.
- [x] Confirm that each frame handler returns and captures 2050 bytes.
- [x] Record wall time and profile the command.
- [ ] Repeat the baseline against the Digitakt II 1.16 snapshot and save the
  summary under `out/emuperf/`.
- [ ] Record the host, Python, Unicorn, firmware, section, snapshot, and Git
  revisions in the benchmark JSON.

Current reference measurement:

```text
three fresh conditions, one pass each, 1,000,000-instruction safety limit
wall time: 1.18 s
captured frame length: 2050 bytes per condition
```

Profiling attributed nearly all measured time to repeated setup: SysEx/flash
construction, scoped ISA-hook installation, and snapshot restore. Actual frame
handler execution was a small fraction of the run.

Example bounded command:

```sh
uv run python tools/machinecommit.py snapshots/boot400M.snap \
  --syx Digitakt_II_OS1.15C.syx \
  --passes 1 --limit 1000000 \
  --json out/emuperf/machinecommit-1.15C-baseline.json
```

**Exit criterion:** the baseline command, metadata, captured lengths, stop
reasons, and timing are saved under `out/` and can be reproduced.

## Phase 1 — Make repeated experiments cheap

**Purpose:** accelerate experiment sweeps without weakening their evidence.

- [ ] Add a focused benchmark that measures build/restore time separately
  from guest execution time.
- [ ] Cache immutable SysEx decode/container/flash results by source SHA-256
  within one process.
- [ ] Cache the MAIN OS pre-scan used to locate scoped FF1/MOVEC hooks.
- [ ] Build one configured `Machine` and restore the same snapshot into it
  between `base`, `inplace`, and `invalid` conditions.
- [ ] Install invariant hooks once; reset only per-condition capture state.
- [ ] Verify that restored host components, registers, memory, and hook-owned
  state are identical before each condition.
- [ ] For event-driven tools, stop at the expected return/driver hook and use
  a wall timeout as the safety bound instead of paying Unicorn's `count=` tax
  for the entire run.
- [ ] Retain an exact counted mode for coverage, instruction-count, and
  determinism claims.
- [ ] Add regression tests proving that old and accelerated modes emit the
  same frame bytes and stop reasons.

Expected gains:

- Reusing setup should reduce repeated-condition startup by roughly 2–3×.
- Avoiding `count=` can recover the measured difference between approximately
  2.0M counted and 15.5M uncounted instructions per second on long runs.
- Native soft-float interception can raise state-oriented runs from roughly
  2M to 20M guest instructions per second, but its instruction counts are not
  comparable with precise runs.

**Exit criterion:** the optimized sweep is byte-identical to the baseline,
has the same endpoint checks, and reports setup and execution timing
separately.

## Phase 2 — Instrument the ColdFire producer narrowly

**Purpose:** prove the producer-side data path without a global instruction
trace.

- [x] Restore the Digitakt II 1.16 `boot400M.snap` with the frame gate open.
- [ ] Add range-scoped memory hooks for the selected track's:
  - source object machine-type byte,
  - DSP mirror row,
  - sync-cache slot,
  - final frame word at `0x94 + 2 * track`.
- [ ] At each relevant access, record PC, direction, size, value, SP, and a
  small bounded stack window.
- [ ] Hook the frame handler and DSPI2 driver entry/return points.
- [x] Capture baseline and one-variable machine-type changes from fresh state.
- [ ] Repeat for at least two track indices to confirm the `2 * track` term.
- [x] Separate direct frame construction from cache invalidation/row refresh;
  an inactive control must not be treated as evidence.
- [ ] Resolve each observed PC back to the image and preserve raw instruction
  bytes in the uncommitted report.

Do not add a global code or memory hook. Every probe must cover a specific
address range or code endpoint and answer a stated provenance question.

**Exit criterion:** a report under `out/` shows the exact ColdFire read/write
chain from track state through the mirror row to the transmitted frame, with
an active control demonstrating that the experiment can observe a change.

The track-0 direct-refresh back half is calibrated in
`out/experiments/a2-machine-provenance/a2-real-005/report.json`: the changed
versus unchanged direct-refresh comparison differs only at row offset 0 and,
after the observed one-cycle delay, at TX `0x95` plus the derived flag at
`0x73d`. This does **not** meet the phase exit criterion by itself: the direct
call bypasses the panel-driven setter-notification/cache path. The unchecked
items remain required follow-up, including input-driven provenance and the
`2 * track` term on a second track.

`out/experiments/panel-machine-commit/qualify-1.16-001/report.json` closes the
input side through `FUN_40051712`: four repeatable runs write track 0 type 2
from the real panel path, but hit `FUN_4002d438` zero times. The next missing
event is not another UI gesture. The producer is now identified as
SSI0-paced eDMA channel 50 completion -> INTC1 source 42/vector 170 -> the
firmware's `INTFRCH1` bit-31 software force -> source 63/vector 191. SSI0,
eDMA48/50 scatter/gather, and safe interrupt-force delivery now have a narrow,
opt-in exact event source. With no fabricated RX data it repeatedly reaches
the generic vector-170 handler, which is waiting for an external
`0x007fffff` sync marker before it installs the pending normal channel-50
callback. The board's external SSI request cadence is also unresolved, so the
CLI deliberately requires an explicit exploratory rate. A host-patched vector
slot proves the downstream guest CINT/force/vector-191 chain only as
calibration. Do not replace the missing RX handover with that control or a
direct function call and call the phase complete.

## Phase 3 — Build concrete differential frame fixtures

**Purpose:** give SHARC analysis exact inputs rather than symbolic guesses.

- [ ] Capture a baseline frame from fresh state.
- [ ] Capture frames after changing only the selected track's machine type.
- [ ] Include at least two valid machine types and one unchanged control.
- [ ] Repeat one pair on a second track.
- [ ] Diff each pair and list all changed ranges, not only `0x94`.
- [ ] Record firmware/section/snapshot hashes, profile name, track, type,
  frame length, frame SHA-256, stop reason, and capture command.
- [ ] Confirm that the expected word is big-endian at `0x94 + 2 * track`.
- [ ] Keep raw frame files under `out/sharcframe/`; commit only tooling and
  non-copyright metadata when appropriate.

Suggested 1.16 capture shape:

```sh
DT2_SECTIONS=out/sections/dt2-1.16 \
DT2_SYX=Digitakt_II_OS1.16.syx \
uv run python tools/sharcframe.py \
  out/snapshots/dt2-1.16/boot400M.snap \
  --syx Digitakt_II_OS1.16.syx \
  --open-gate --passes 1 --limit 5000000 \
  --out-dir out/sharcframe/dt2-1.16 \
  --json out/sharcframe/dt2-1.16/baseline.json
```

**Exit criterion:** a deterministic fixture set shows which frame bytes vary
with machine type and which remain invariant.

## Phase 4 — Bridge captured frames into SHARC tracing

**Purpose:** combine dynamic ColdFire evidence with bounded static SHARC
execution.

- [ ] Identify the SHARC receive buffer or the earliest routine that receives
  its pointer using loader seeds, call sites, and the public hardware manuals.
- [ ] Add an optional concrete-memory input to `tools/sharc_trace.py` or a
  small adapter around it; do not make firmware bytes a committed fixture.
- [ ] Map the captured frame at an explicitly supplied SHARC address/base.
- [ ] Preserve symbolic `track_index` while making invariant frame bytes
  concrete.
- [ ] Emit memory events with both effective address expressions and concrete
  values when available.
- [ ] Seed complete caller state, including relevant R/I/M registers, instead
  of restarting with important pointers uninitialized.
- [ ] Add lightweight compare/bit-test predicate tracking so known conditions
  do not fork both ways unnecessarily.
- [ ] Add focused tests using synthetic, original byte arrays rather than
  firmware-derived frame contents.

**Exit criterion:** a bounded trace can load known bytes from a supplied frame
and retains an affine address expression for the selected track.

## Phase 5 — Prove the SHARC machine-type read

**Purpose:** establish the first verified DSP-side consumer.

- [ ] Trace from the earliest justified receive-path seed.
- [ ] Find a memory access equivalent to
  `receive_buffer + 0x94 + 2 * track_index`.
- [ ] Confirm its instruction start and bytes against the loader image.
- [ ] Confirm the decoded operation against public SHARC+ documentation.
- [ ] Follow the loaded value far enough to distinguish machine selection,
  validation, copying, or unrelated bookkeeping.
- [ ] Check callers/references with both Ghidra data and raw-image scanning
  before making absence claims.
- [ ] Have a second agent independently check the address arithmetic and
  image bytes.
- [ ] Record the result in `docs/FINDINGS.md` with the correct evidence mark.

**Exit criterion:** `docs/FINDINGS.md` contains a second-agent-checked finding
identifying the SHARC instruction, function, input buffer provenance, affine
address, loaded width/value, and immediate downstream use.

## Phase 6 — Decide the next reverse-engineering seam

Only begin this phase after Phase 5 succeeds.

- [ ] Locate the machine dispatch, table lookup, or state field fed by the
  loaded type.
- [ ] Compare Digitakt II and Digitone II implementations to separate common
  transport code from product-specific machine handling.
- [ ] Decide whether the next useful seam is machine dispatch, parameter
  layout, initialization, or modulation routing.
- [ ] Write a separate bounded plan for that seam rather than expanding this
  plan into full DSP emulation.

## Stop and pivot rules

- If a candidate function loses receive-buffer or track-index provenance,
  stop tracing it rather than implementing every instruction it reaches.
- If two consecutive unsupported instructions are encountered without stronger
  target provenance, classify the function before adding more semantics.
- If `0x1c0cd0` proves to be a generic byte-copy/fill helper, record that and
  leave it; do not complete its instruction coverage merely to continue.
- If the SHARC receive-buffer base remains unknown after Phase 3, pivot to
  DT2/DN2 normalized function matching and DSPI2 receive-path analysis.
- Use bounded 1.15C dynamic observation only when static analysis cannot
  resolve a boundary fact; compare the `.syx` and section hashes first.
- Do not build a full SHARC emulator solely to answer the machine-type
  question. Reconsider that only if several later DSP questions share enough
  execution requirements to justify the cost.

## Completion checklist

- [ ] ColdFire producer provenance is reproducible.
- [ ] Differential frames and metadata exist under `out/`.
- [ ] Repeated experiments are fast enough for iteration.
- [ ] Concrete frame bytes can be consumed by the SHARC tracer.
- [ ] The affine machine-type address is proven on SHARC.
- [ ] A second agent has checked the image bytes.
- [ ] The verified result is recorded in `docs/FINDINGS.md`.
- [ ] The next machine-dispatch investigation has its own bounded plan.
