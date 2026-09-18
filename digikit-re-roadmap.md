# DigiKit reverse-engineering roadmap

## 1. Mission, target, safety, and evidence contract

DigiKit investigates **safe modification surfaces** in Digitakt II (DT2) and
Digitone II (DN2): where code or data can change, which processor owns a
behaviour, what fixed limits and persistence rules apply, and how a changed
image can be delivered safely. It is not a wholesale decompilation project.

### Analysis targets and hardware boundary

- Static and emulator-analysis targets are **DT2 1.16** and **DN2 1.11**.
- The physical DT2 remains on **1.15C**. Installing 1.16 upgrades its bootstrap
  irreversibly; any hardware test of a 1.16-derived build requires Em's
  explicit irreversible-bootstrap decision.
- Use 1.15C only for the existing physical-device context or a bounded dynamic
  comparison when static evidence cannot answer the boundary fact. Do not
  silently rebase new claims or tools to it.
- Every address-dependent tool must identify the image/profile and enforce
  byte preconditions where applicable. An address copied between images is not
  evidence.

### Evidence and repository rules

- Record findings in `docs/FINDINGS.md` using **[V]**, **[D]**, **[O]**, and
  **[C]**. A **[V]** needs an independent second-agent check against image
  bytes. Use **[D]** for carried or read-once documentation, **[O]** for open
  work, and **[C]** for corrections.
- Reports separate **OBSERVATION**, **INFERENCE**, and **HYPOTHESIS**. Do not
  use a maturity ladder such as `UNKNOWN → HYPOTHESIS → LIKELY → CONFIRMED`.
- Firmware and firmware-derived data remain ignored: `.syx`, sections, raw
  captures, snapshots, Ghidra dumps, disassemblies, and measurement databases.
  Commit recipes, schemas, synthetic fixtures, tools, tests, and non-derived
  metadata only.
- One variable, an explicit baseline, focused RAM/frame differences, and narrow
  instrumentation are the default. Instrumentation can change behaviour.
- Do not assume processor ownership. Establish it for each vertical slice.
  Use LLMs as falsifiable investigators: bounded evidence, competing
  explanations, and the smallest discriminating experiment—not authority.

### North star

> **What is the smallest experiment or byte-checked static trace that
> distinguishes the competing explanations of this behaviour?**

The portfolio succeeds when it can state a modification surface concretely:
owner, source state, representation, consumer, extension point, fixed limits,
persistence effect, delivery path, and remaining evidence gap.

### Current authorities and known debt

Start with current findings and narrow plans, not old phase labels:

- `HANDOVER-2026-09-16-machine-to-dsp.md` — latest root handover baseline for
  machine-to-DSP work, not unqualified current state or order. Later
  `docs/FINDINGS.md` corrections, current source, and
  `docs/plans/EMULATOR-SHARC-BOUNDARY.md` override it where they conflict,
  especially its pre-queue-correction 1a/1b model.
- `docs/FINDINGS.md` — especially machine type at TX `0x94 + 2i`, row refresh,
  queue-drain correction, LFOs/modulation, and the SHARC side of SPI.
- `docs/TOOLS.md`, `docs/plans/EMULATOR-SHARC-BOUNDARY.md`, and
  `docs/PATCHING.md` — executable methods and guardrails.

Older `README`, `docs/HANDOVER.md`, and `docs/NEXT.md` may also be stale.

## 2. Capability baseline: established, bounded, and incomplete

This is a capability ledger, not a claim that a sequence of broad phases is
complete.

| Established capability | Limitation / decision still open | Authoritative path |
| --- | --- | --- |
| Snapshots/checkpoints and bounded ColdFire runs support controlled comparisons. | Static/hardware delivery is incomplete; each run needs image identity and bounded endpoint. | `docs/TOOLS.md`; boundary plan |
| Focused RAM and frame diffs can isolate a one-variable change. | — | findings: row refresh and frame map |
| The byte-checked static chain and bounded direct-refresh experiment establish source `+0xa2` -> SRAM row -> TX `0x94 + 2*track` on DT2 1.16. | The indirect setter-notification/cache-invalidating front half and faithful 1.16 UI replay remain open; neither is evidence for the other. | findings; `a2-real-005`; boundary plan |
| Encountered MCF5441x peripherals have a documentation-backed contract and Ghidra labels. | This is encountered-peripheral coverage, not a complete chip model. | `docs/TOOLS.md`; findings |
| Frame fields are verified, including per-track machine type at TX `0x94 + 2i`. | ColdFire transmission does not prove the SHARC consumer or unknown-type behaviour. | findings; handover |
| The SHARC loader, decoder, mapped memory view, and bounded affine tracer exist. | The tracer lacks the concrete input/predicate support required for the receive proof. | `docs/TOOLS.md`; boundary plan |
| ColdFire redirects/trampolines were demonstrated on DT2 1.15C and DN2 1.10E. | Neither demonstration establishes safety or applicability for current DT2 1.16 or DN2 1.11 targets. | `docs/PATCHING.md` |

The durable method is vertical slices: prove only enough of one end-to-end
behaviour to decide a modification question, then retain the evidence and
constraints in the feasibility matrix.

## 3. Workstream A — Machine-selection boundary

**Decision question:** after a track type reaches the frame, does the SHARC
copy, validate, dispatch on, or transform it—and does that require type
substitution or DSP modification for a new machine?

ColdFire producer/wire evidence is established. A0/A1 established the
controlled emulator laboratory on 1.15C, and a hash-gated 1.16 direct-refresh
control established the narrow source-object -> SRAM row -> frame back half.
A2 remains open until the real panel gesture drives that path on 1.16. This is
deliberately compositional: A1's menu-open gesture and a direct function call
are controls, not substitutes for an input-driven machine commit.

### Gates A0–A5

| Gate | Required result | Evidence / stop rule |
| --- | --- | --- |
| **A0 — deterministic runner (Phase 1 complete)** | Use one exact JSON recipe to repeat a fresh baseline and one press/release gesture from the same snapshot, saving endpoints and an explicit RAM diff. | Hash-match the selected sections and firmware; failed/missing endpoints are failures, not repeatability. Do not generalize this thin runner. |
| **A1 — dynamic ColdFire A/B (complete)** | Choose one UI gesture and establish its repeatable RAM, panel-frame, coverage, and call differences against the baseline. | `a1-real-002` validates the FUNC+SRC experiment in quarantined state/profile lanes; observations remain separate from ownership inference. |
| **A2 — narrow provenance** | Replay the qualified real-panel machine commit on DT2 1.16 and trace setter/notification/invalidation, `FUN_4002d438`, the mirror row and TX `0x94`. | `a2-real-005` is the direct-refresh calibration/control. Acceptance requires input-driven source mutation and row/frame propagation without host pokes or direct calls, with clean/traced endpoint equivalence. |
| **A3 — static-to-SHARC handoff** | Use the dynamic frame/provenance result to justify the earliest SHARC receive-buffer seed and complete relevant caller state. | Follow SPI2 receive evidence, loader map, and byte-checked instruction starts. Do not seed an arbitrary nearby routine. |
| **A4 — tracer and exact read** | Add only concrete-memory/predicate support needed to preserve the dynamically justified `receive + 0x94 + 2*track` address and prove immediate use. | Synthetic tests and independent loader-byte checks; stop if receive or track provenance is lost. |
| **A5 — design decision** | Classify the use as copied, validated, dispatched, transformed, or another evidenced operation; then choose unknown-type handling, stock-type substitution, or DSP modification. | No decision before A4. “SHARC consumer solved” is not currently valid. |

The byte-checked static producer/wire chain remains the baseline check, but the
blocked SHARC receive seed is deliberately behind A0–A2 dynamic emulator
evidence. The bounded plan is `docs/plans/EMULATOR-SHARC-BOUNDARY.md`; extend
it rather than duplicating its commands here.

## 4. Workstream B — ColdFire machine template and static delivery

**Decision question:** can a profile-driven template produce an image-specific,
preconditioned DT2/DN2 machine extension and deliver it through the software
path?

1. Thread `tools/machineprofile.py` profiles through `machinepatch.py`; remove
   build-specific constants while retaining existing synthetic plan tests.
2. Generalize stock machine counts and table widths from the selected profile.
3. Make product-specific tables optional and keep DN2 parameter-page work
   separate: DN2 has a product-specific parameter-page layer, so page work may
   be required; do not assume a descriptor/name extension alone is sufficient.
4. Produce a static MAIN OS patch with image/profile identity and expected-byte
   checks; validate using `patchimg`, container `roundtrip`, and a bounded boot.
5. Hardware delivery is blocked by Em's explicit bootstrap decision. Software
   validation does not authorize installation.

Redirects/trampolines are demonstrated on DT2 1.15C and DN2 1.10E, but neither
proves safety or applicability for current DT2 1.16 or DN2 1.11 targets; see
`docs/PATCHING.md`.

## 5. Workstream C — LFO and modulation vertical slice

**Decision question:** can an LFO destination or another LFO be extended
without violating owner, representation, or DSP constraints?

Existing evidence places three LFOs on ColdFire and identifies derived values
in the frame. Trace one destination from UI/configuration through modulation
state and derived frame field to its final consumer. Then decide whether the
routing is generic/data-driven, fixed-size, DSP-configured, or DSP-computed.

Do not infer a fourth LFO's feasibility from three existing ones. Record array
bounds, destination encoding, persistence, and DSP impact in the matrix.

## 6. Workstream D — Machine-parameter vertical slice

**Decision question:** can one machine parameter be extended safely and
persistently?

Trace exactly one parameter through:

```text
UI/descriptor → state → dirty or mirror representation → frame/consumer → persistence
```

Determine descriptor ownership, parameter-page/UI limits, state width and
range logic, dirty propagation, transmitted representation, final consumer,
and saved-project representation. This workstream is complete only when those
constraints support a decision; naming intermediary functions is insufficient.

## 7. Workstream E — Safe patch surfaces

For each target image, make a separate evidence-backed map of candidate cave,
data/table, and code-redirect surfaces. A candidate needs image identity,
precondition bytes, static references, computed-reference review, relevant
runtime observations, loader/layout constraints, alignment/branch/cache
constraints, and rollback/delivery implications.

Never generalize DT2 1.15C cave evidence to DT2 1.16 or DN2. The 1.15C tail
showed why zero bytes and one clean boot are not proof of free space; use the
multi-method limits in `docs/PATCHING.md`.

### Controlled patch progression

Progress only as evidence permits:

1. constant or data change;
2. range/default change;
3. branch or condition change;
4. redirect an existing handler while preserving its behaviour;
5. small trampoline or bounded injected ColdFire function;
6. descriptor/table extension;
7. larger subsystem extension;
8. DSP-side modification only if the preceding gate requires it.

Each rung needs image-specific expected bytes, software validation, and a
recovery-aware delivery assessment. No rung implies a hardware test.

## 8. Feasibility and delivery matrix

Update this matrix after every decision-quality experiment. `?` means an open
constraint, not a promise. Evidence marks refer to the current findings record.

| Modification | Processor owner | Product/build scope | Extension point | Limits / persistence | DSP impact | Software validation | Hardware decision | Evidence | Next experiment |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Change parameter range | ColdFire-led; final consumer ? | DT2 1.16, DN2 1.11 separately | Descriptor/range logic ? | Width, clamps, save format ? | Consumer may receive derived value | Profiled data patch, roundtrip, bounded boot | Blocked for 1.16 | [O] | D: trace one parameter |
| Add LFO destination | ColdFire configuration; final owner ? | DT2/DN2 separately | Destination descriptor/routing ? | Fixed destination count and persistence ? | Frame-derived values known; interpretation ? | One destination slice and bounds tests | Blocked for 1.16 | [D][O] | C: trace one destination |
| Add another LFO | ColdFire has three existing LFOs | DT2/DN2 separately | Arrays/task/UI ? | Fixed counts, derived fields, save format ? | May require new DSP input/compute | Synthetic bounds plus bounded patch ladder | Blocked for 1.16 | [D][O] | C: establish current array/consumer |
| Augment a machine | Split ColdFire/DSP ? | DT2 template then DN2 page variant | Descriptor plus parameter mapping ? | Per-type tables/pages, project save ? | Type already crosses frame | Profiled template + static patch validation | Blocked for 1.16 | [D][O] | B then D/A4 |
| Add a machine as stock-type clone | ColdFire template; DSP type use unknown | DT2 1.16 first; DN2 distinct | Profiled tables, optional DN2 pages | Counts, table shape, persistence ? | Substitution may avoid new DSP code; unproven | `patchimg` + roundtrip + bounded boot | Explicit Em decision | [D][O] | A4/A5 then B |
| Add a new DSP machine | DSP dispatch/loader ? | Per image/product | Dispatch/code-load surface ? | Executable map and resources ? | Likely; not established | Only after A5 and per-image surface proof | Explicit Em decision | [O] | A4: prove immediate use |
| ColdFire code redirect | ColdFire | Demonstrated on DT2 1.15C and DN2 1.10E; not shown safe/applicable for DT2 1.16 or DN2 1.11 | Per-image callsite/cave | Cave ownership and reachability per image | None unless behaviour crosses frame | Preconditions, bounded boot, preserved-call checks | No automatic authorization | [V] for demonstrated builds only | E: map target image |

## 9. Emulator-first experimental platform

The emulator is a first-class primary workstream, not deferred infrastructure.
Use deterministic UI-driven A/B experiments from a common snapshot: each has a
fresh repeated baseline, exactly one logical gesture, fixed instruction counts,
and ignored endpoint artifacts. Compare RAM, panel frames, coverage, and calls;
then use narrow read/write watchpoints only where a diff identifies a
candidate seam. This dynamic ColdFire evidence is the handoff for the static
SHARC receive investigation, not a substitute for its later byte checks.

Phase 1 is complete: `tools/experiment.py` runs one named JSON recipe through
`guirun.py`, checks sections-versus-firmware identity, records commands and
endpoint hashes, verifies that both paced gesture transitions were delivered,
and emits a structured selected-range diff. The bounded validation run is
`out/experiments/phase1-button/phase1-real-004/`: both repeated cases saved at
instruction 12,168,354 with matching within-case hashes and zero faults. Its
selected SRAM range had zero changed bytes; that is an A0 runner result, not a
claim that the button had no effect. The tool is purposely not a general
framework. A generic experiment format, orchestration layer, or shared metadata
schema remains deferred until at least **two active experiments** show measured
duplication and tests show preserved behaviour. Store raw outputs under ignored
paths; version-control only recipes, schemas, synthetic fixtures, and
non-derived metadata.

Retain bounded, demand-driven dataflow. Add tracer semantics only to answer an
active gate with a synthetic regression test. Do not broaden this into global
taint tracking, full emulation, a protocol corpus for its own sake, or a
universal experiment runner.

## 10. Recommended execution order

**Recommended pending user change:** establish the DT2 gate first, then make a
bounded DN2 comparison where it can distinguish common transport from
product-specific behaviour.

1. **A0 (complete):** the deterministic button recipe now has a
   hash-consistent repeated validation run and explicit SRAM diff.
2. **A1 (complete):** `a1-real-002` establishes repeatable mapped-memory,
   panel-frame, block-entry and scoped UI-call differences for FUNC+SRC.
3. **A2 (current):** faithful 1.16 panel input now reaches the relocated setter.
   Connect its notification/invalidation front half to the calibrated
   row/frame back half. `a2-real-005` is a control, not completion of this gate;
   its producer is statically recovered as SSI0/eDMA50 vector 170 followed by
   an `INTFRCH1` software force. The narrow opt-in event source now reaches
   the generic vector-170 handler, but external SSI cadence and RX sync-marker
   data are still unknown, so the natural callback handover does not occur.
4. **A3/A4:** hand the input-driven frame/provenance result to the static SHARC
   receive seed, then prove the exact read and immediate use with byte checks.
5. **A5:** decide copied/validated/dispatch/transformed and then the
   substitution-versus-DSP path.
6. **B:** profile-threaded machine template and static software delivery.
7. **C:** one LFO-destination vertical slice.
8. **D:** one machine-parameter vertical slice.
9. **E:** target-specific patch-surface evidence for the next required rung.

Cross-version/product methods and platform improvements support these gates;
they are not late phases or a linear earliest-incomplete-phase schedule.

## 11. Completed implementation tranche — Phase 1 emulator runner

The narrow deterministic runner is implemented and validated: one named recipe,
a fresh exact baseline and one button press/release from the same snapshot,
repeated endpoint hashes, verified delivery of both gesture transitions, and a
selected-range RAM diff. It requires the sections source hash to match the
chosen firmware before every real run and preserves raw logs, snapshots, and
diffs only under ignored `out/experiments/`.

The direct-refresh calibration is complete in
`out/experiments/a2-machine-provenance/a2-real-005/`: for DT2 1.16 track 0,
the controlled source-type byte is the sole changed row byte and reaches TX
`0x94` after one observed frame cycle. Faithful 1.16 panel execution and setter
mutation are now qualified in
`out/experiments/panel-machine-commit/qualify-1.16-001/`; the equivalent,
shorter preferred schedule is
`out/experiments/panel-machine-commit/qualify-1.16-fast-001/`. Independent
exact runs scale through eight workers on the current host without changing
endpoint hashes (`out/benchmarks/exact-workers-1.16-001/report.json`). The
exact next work remains A2: obtain the external SSI cadence and RX
`0x007fffff` synchronization provenance needed for the generic vector-170
handler to install the normal eDMA50 callback, then trace
notification/invalidation through the now-calibrated CINT/force machinery
into the refresh/row/frame back half. A host-patched callback control proves
normal vector 170 -> software-forced vector 191 but is not behavioral proof.
A3 remains blocked until this input-driven provenance exists.

## 12. New-session handoff prompt

> With the deterministic emulator **A0** runner, accepted **A1** FUNC+SRC
> A/B experiment, faithful DT2 1.16 panel -> setter qualification, and direct
> source -> row -> frame calibration complete, continue **A2** by recovering
> the external SSI cadence and RX synchronization needed for the new narrow
> eDMA48/50 model's generic vector-170 handler to install the normal callback.
> Then trace the natural setter -> notification/invalidation -> refresh ->
> row -> frame path. Do not start A3
> until that input-driven provenance is recorded. Read
> `HANDOVER-2026-09-16-machine-to-dsp.md`, current headings in
> `docs/FINDINGS.md`, `docs/TOOLS.md`, and
> `docs/plans/EMULATOR-SHARC-BOUNDARY.md` before consulting stale plans.
> The mission is safe DT2/DN2 modification surfaces, not wholesale
> decompilation.
>
> Target DT2 1.16 and DN2 1.11. The physical DT2 remains on 1.15C; do not
> propose a 1.16 hardware test without Em’s explicit irreversible-bootstrap
> decision. Require image identity/profile and byte preconditions for every
> address-dependent action. Keep firmware-derived artifacts ignored; commit
> only tools, recipes, schemas, synthetic fixtures, and non-derived metadata.
>
> Use one variable, repeated fresh-snapshot baseline comparisons, fixed
> instruction endpoints, RAM/frame/coverage/call diffs, and only narrow
> diff-selected read/write instrumentation. Keep processor ownership neutral.
> For each report, separate **OBSERVATION**,
> **INFERENCE**, and **HYPOTHESIS**; findings require [V]/[D]/[O]/[C], and [V]
> requires an independent second-agent byte check. LLM suggestions must state
> falsifiable alternatives and the smallest discriminating experiment.
>
> Report: gate and decision question; image/profile/preconditions; exact
> baseline/manipulation commands and repeatability; bounded stop condition;
> RAM/frame/coverage/call observations; INFERENCE; HYPOTHESIS; evidence mark or
> reason it is not a finding; changed tools/tests (if any); ignored outputs;
> residual limits; and the single next experiment. Stop rather than broaden
> the runner or start SHARC static work before the dynamic handoff is
> defensible.
