# Sampler: resume with a bounded hardware observation

## Checkpoint and direction

Source worktree: `/private/tmp/digikit-dsp-sampler`, branch
`sdk/dsp-sampler-discovery`, clean and fully pushed at `63a6ba13` before this
handover. Earlier checkpoints: `6623d5a8` (SSI), `0f38f288` (mask commands).
The DTIM tick checkpoint passed 60 named tests, independent static review and
scoped path/layer checks. The full merge gate remains deferred; full native
fault-history acceptance remains failed/open. No hardware proof follows from
these component results.

The current direction is in the 2026-11-01 checkpoint of the
[master plan](../plans/dsp-sdk-and-sampler.md). This handover authorizes no
hardware actions or flashing. One owner must coordinate device access and
preserve the other Claude checkout, edits and PIDs.

Read the [master plan](../plans/dsp-sdk-and-sampler.md),
[previous handover](HANDOVER-2026-10-30-stereo-sampler.md) and
[finding 107](../findings/107-storage-interrupt-emulation-scope.md) for prior
commands, evidence and limits. Firmware inputs and generated `out/` artifacts
are ignored and absent from the remote; tracked plans/findings preserve the
commands and evidence references, not the artifacts themselves.

## Product requirements and remaining work

Keep full stereo on DT2 OS 1.17 and DN2 OS 1.12, sixteen tracks sharing one
deduplicated project pool, and long samples bounded by real capacity/format
limits. There is no ten-second or 16 MiB product cap. Selecting a sample stops
only that track's old notes. Keep one shared SDK/sampler engine with
runtime-shipped device adapters. No production reservation is selected.

Open work includes backing ownership/coexistence and reader lifetime, native
fault-history proof, full stereo integration, per-track binding, and the
on-device picker/end-to-end flow. Reusable listing passes on both devices;
the picker is unfinished. DT2 observations cannot establish DN2 hardware parity.

## Concrete gap map for the hardware session

These are not all instruction-emulation bugs. Separate missing model behavior
from firmware ownership/lifecycle questions and unmeasured hardware behavior.
Remote control reproduces the scenario; dedicated telemetry supplies evidence.

| Priority / area | Gap and source | Useful observation |
| --- | --- | --- |
| First: ColdFire storage reset | Post-RSTA IRQSTATEN is answered statically: the model sets `0x117f013f` (finding 107); only the exact full value is open. [Finding 107](../findings/107-storage-interrupt-emulation-scope.md) and [eSDHC model](../../packages/emulator/periph/src/esdhc.rs). | Optional: read IRQSTATEN/IRQSIGEN after the firmware's normal reset completion and initialization, tagged with event order, to confirm the full value. Do not manually reset an active storage controller. |
| ColdFire delivery/order | Default Oracle storage completion bypasses real handlers; isolated Device handlers/waiters pass, but native composition of storage, timers, SSI and host inputs is unfinished. [Board](../../packages/emulator/machine/src/board.rs), [Time](../../packages/emulator/machine/src/time.rs), [native runtime](../../packages/emulator/boot/src/runtime.rs), [finding 107](../findings/107-storage-interrupt-emulation-scope.md). | Small records of command issue, DMA/controller ISR entry, captured status, acknowledgement and waiter continuation during a known sample load. This provides an ordering reference, not complete interrupt coverage. |
| SHARC cache/DMA visibility | Answered statically for the stock path and a CPU-copy design ([finding 108](../findings/108-dt2-sample-cache-visibility.md)); a device test is needed only for direct DMA into cached memory. | None for the stock path or a CPU copy. For direct DMA: the one probe in finding 108. DN2-specific helper addresses are not DT2 probe addresses. |
| SHARC reader/interrupt lifetime | Frame-fetch tests supply controlled interleavings; physical IRQ/DMA exclusion, callback retirement and complete writer coverage remain open. [Finding 99](../findings/99-stereo-frame-fetch-exclusion.md), [retirement design](../findings/83-sample-retirement-design.md). | Correlate publication, reader admission/exit, stop acknowledgement and reclaim events under a repeatable remote-controlled selection/playback scenario. A quiet trace alone is not an ownership proof. |
| Product memory ownership, not just emulator fidelity | DT2: answered by [finding 109](../findings/109-dt2-stock-pool-borrowing.md) (borrow the stock pool). DN2: measured by the canary soak ([finding 106](../findings/106-dn2-ddr-configuration.md#canary-map-of-stock-ddr-writes-2026-11-01)). [Stock sampler dependencies](../findings/81-stock-sampler-dependencies.md), [DN2 DDR finding](../findings/106-dn2-ddr-configuration.md). | Observe stock DT2 allocation/publication/retirement across repeatable load, replace and project-change scenarios, then reconcile with static firmware evidence. DT2 cannot establish DN2's available pool or ownership. |

Start with the first row. Treat SHARC arithmetic as a separate experiment, and
prioritize cache/lifetime observations for sampler integrity. Do not advertise a
full emulator gap list: this map covers the unresolved sampler-related seams.
Use firmware-query before choosing current-image probe sites; do not transplant
DN2 addresses into DT2 simply because the devices share a DSP family.

## Direction change (2026-11-01)

The direction changed on 2026-11-01. The current state and next steps are in
[docs/plans/dsp-sdk-and-sampler.md](../plans/dsp-sdk-and-sampler.md) (checkpoint 2026-11-01).

## Timer boundary to preserve

Ordinary Device DTIM completed post-prescaler ticks are implemented and supported
by the manual's DTRR+1 interval and immediate FRR reset passages; no hardware cycle
accuracy or native clock integration is established. PIT counter/phase advancement
is **not implemented**. Manual p1158 wording suggests PIF at the 1→0 transition;
a PMR+1 period formula alone does not settle initial phase. Inspect the waveform
visually before implementing PIT phase. Do not copy the DTIM phase by analogy.
See [finding 107](../findings/107-storage-interrupt-emulation-scope.md).
