# Private DT2 storage-fault observer

Opt-in `core/storage-fault-proof` targets only DT2 OS 1.17. It is not in a default
module set, a sample-load gate, recovery mechanism or public module ABI.

Three `replace` hooks target the audited six-byte IRQSTAT→D0 loads in storage ISR
`0x4012d5ba`: offsets `0x08`, `0x5c`, `0x88`. Each reads MMIO `0xfc0cc030` exactly
once, ORs only bits `0x00310000` (command timeout, data timeout, CRC) into the
four-byte `storage_fault_bits`, and jumps to its named site plus six. It must never
jump to a displaced-code tail, which would perform the read twice. D0 and the
load's N/Z, cleared V/C and unchanged X are retained, with all other registers,
full SR control, A7 and the existing exception frame preserved.

Each hook uses 13 instructions and eight bytes below A7: saved D1 at SP+0 (four
bytes) and saved post-load SR at SP+4 (two bytes), with two unused bytes. LEA and
MOVEM precede the SR snapshot without changing CCR. The one memory-destination
OR.L merges the current sticky word in a single CPU instruction; an observer
running between instructions cannot lose another observer's bits. This does not
cover concurrent non-CPU writers or prove whole-ISR/controller behavior. No IRQ
mask change, heap, logging, firmware call, counter or clear operation is added.

The linker-owned BSS word is initially zeroed by the existing expansion segment
loader, before the traced MAIN storage initialization. It is separate from
runtime-loaded DKM BSS and is never reset by selection/load/UI/runtime hooks.
Bounded native traces observe loader zeroing before FS initialization and host-seeded
retention, but the full fault-history acceptance remains failed/open: it records
zero observer writes. The observer covers only these three snapshots and audited error bits;
quiet output is not health, cache-history, full fault coverage or DMA-drain proof.

The compiled-source owner test is `packages/patchkit/tests/storage_fault_observer.rs`:
`cargo test -p patchkit --test storage_fault_observer` with an installed ColdFire
toolchain, under the shared benchlock and an isolated host target. It assembles and
links this actual source, then uses Cpu/custom Bus checks for MMIO-read count,
context/exception frame, exact write footprint, sticky union and observer nesting
at every instruction boundary. No firmware bytes are stored in these tests.

A nonzero readiness value must be recorded and classified, not assumed to be an
unsafe sample-source failure: startup might contain handled probe errors. Native
acceptance must observe loader zeroing and sticky accumulation rather than assume
zero at ready. This raw observer does not classify all captured bits as unsafe
runtime source faults or implement the accepted sample-load policy.

Independent static review of the patched image checks the actual segment chain,
not only the plan: adapter payload 164 bytes at `0x47e8b000`, BSS four bytes at
`0x47e8b0a4`; DKPR payload 92 bytes at `0x47e8b0a8`, no BSS. Reset `0x4000053e`
reaches `pk_loader` at `0x403117c4`; loader relocation starts at stock MAIN end,
and its final helper calls reach `0x4000045c/0x400004b2`. Patched MAIN SHA-256 is
`6ba724b86296c238e068806c355af8f180f9c43eb5909f1b5ea331198a84c559`.
These are static artifact checks. Default native storage completion uses Oracle
semaphore posting rather than establishing stock ISR execution; the full native
fault-history check remains **failed/open** with zero observer writes.

The opt-in [controlled ISR fixture](../../../../packages/patchkit/tests/storage_fault_isr.rs)
loads the exact stock DT2 1.17 `.syx` at runtime (SHA-256
`26c22f6652625ac2cfd47f7ee970d388ed8b6427dae3563c0a6a2f2d334350d5`),
verifies each original hook-site hash and installs assembled hooks only in the
in-memory copy. It executes the actual stock ISR, both signal helpers on their
no-waiter path and the ISR's RTE. Synthetic vector/level setup enters through
`Cpu::take_interrupt`; a sparse bounded Bus supplies a controlled W1C IRQSTAT
test double. No firmware bytes are embedded.

The named test passed 36 sequence/SR combinations: eleven single-interrupt cases
and one five-interrupt sequence, each with three SR values, for 48 interrupt
invocations per image (stock and patched). It checks each audited error bit,
completion-only and simultaneous error/completion cases, and sequential sticky
union. Stock/patched register/SR/PC/A7 results, exact MMIO reads/W1C writes,
nonobserver write traces and status/signal globals match; the observer latch and
its checked temporary stack writes are excluded from the write comparison.
Each replay has a 2,048-instruction ceiling.

The same fixture now also exercises isolated controller delivery and an actual
waiter on exact DT2 1.17 and DN2 1.12. DN2 input SHA-256 is
`7e5b8a733d4ab1fdf7e5d22757f7d8eddaa3cbf71d0858874a791844c02ceb35`.
The shared observer is linked directly to verified DN2 ISR sites in memory for
this test only; `module.toml` remains DT2-only, with no DN2 packaged support.

`Machine::step_esdhc` requires Device Board and attached Device Time. The fixture
initializes IRQSTATEN/IRQSIGEN, IMR and ICR explicitly and unmasks source 31 through
CIMR; the real shared INTC bank selects vector 223 at level 3 subject to CPU IPL.
Device SIMR/CIMR support is limited to byte writes selecting source 31 or 0. Ordinary
stepping and Oracle completion retain their existing behavior. Configured,
pending, queued or held DMA major completion state, including register-latched
source-0 group requests, is rejected before mutation; `step_esdhc` remains isolated.
The explicit storage-pair route below handles DMA59 separately.

An ordinary guest CMD13 first latches CC while the semaphore waiter remains
blocked, proving no host post before interrupt entry. Immediate controlled Device
fault/TC events then produce four cases: CC, CTOE+CC, TC+DTOE+CC and TC+DCE+CC.
Stock/patched runs on both devices pass 16 actual ISR/RTE invocations and waiter
ready-queue insertions, with 36 entries each at the actual signal wrapper
`0x4000155c` and helper `0x400011ee`. They check semaphore counts/pointers,
ready-head insertion, self-links, high-ready selection, bit-13 scheduling request,
IRQSTAT W1C deassertion and preserved interrupted context. CTOE+CC leaves command
status zero while the observer retains CTOE; TC+DTOE/DCE leaves data status `-1`.
The observer preserves stock status, context and queue results.

In this earlier fixture, the scheduling request is observed without servicing it.
This checkpoint contributes 16 invocations;
the original no-waiter fixture contributes 96. The pair proof below adds 32,
for **144 total across three tests, not per image**. Fixture dependencies remain
local; no external package was added. Independent route/fixture static reviews
reported no issues; evidence remains **[D]**, not **[V]**.

The controlled storage-pair checkpoint now also passes. Explicit
`Machine::step_storage` requires Device Board/Time and offers only INTC2 eSDHC
source 31/vector 223 and the source-0/vector-192 DMA56–63 group, supporting
channel 59 here. It reuses shared IMR/ICR and live CPU IPL, choosing higher level,
then higher source. Equal level 3 delivers 223 then 192; DMA level 4 reverses
that order. Other pending channels 56–63 fail explicitly. Configured/pending
legacy delivery fails before mutation; queued Board completions use the eDMA
register latch without feeding the legacy pending boolean. DMA-written ranges
are invalidated before exception entry or execution. Defaults remain unchanged.

Device completion latches INTH `0xfc044020` bit 27 only for
`completion.done && completion.major_interrupt`; TC follows actual data DMA
completion, not a command data flag alone. Device byte CINT honors NOP bit 7,
then CAIR bit 6 (both interrupt banks), otherwise the selected channel only.
Contained big-endian 1/2/4-byte INTH/INTL writes are W1C; crossing accesses reject
before mutation. Other channel bits and DSPI/SSI dispatch are preserved, as is
Oracle passthrough. DMA requests persist until acknowledgement, not CPU entry.
The existing Board snapshot round-trips the register-backed pending bit without
new format; runtime policy/snapshot compatibility remains a Stage C question.

The new fixture executes guest SERQ59 and CMD18 against deterministic synthetic
media, checking all **512 bytes before the first IRQ and after both handlers**.
Four cases (DMA level 3/4 without added errors, level 3 with CTOE+DTOE, level 4
with DCE), both targets and stock/patched copies produce **16 pair runs, 32 actual
ISR/RTE invocations and 48 waiter insertions**, each bounded to 4,096 instructions.
Counts come from fixed source loops/assertions; the log reports named tests.
Actual firmware CINT clears DMA59 and eSDHC W1C clears IRQSTAT. Vector-entry/RTE
order, preserved context/status, semaphore coalescing and a shared circular
three-node bidirectional ready queue match between stock and patched runs. All
three waiters remain blocked before entry; the real signal helpers insert them.
Scheduling-request bit 13 is observed without running the scheduler. Patched raw
error bits remain sticky; the DN2 observer relocation is in-memory test-only.

Error bits are injected after the genuine copy completes. This is controlled
completion plus error status, not partial/corrupt DMA recovery or physical
quiescence. The later software-force checkpoint below extends arbitration without
adding physical interrupt sources.

From the repository root, using the original checkout's shared benchlock for
this worktree:

```sh
PATH=/Users/em/.cargo/bin:$PATH \
UV_NO_SYNC=1 UV_CACHE_DIR="$PWD/out/cache/uv" \
CARGO_BUILD_JOBS=2 \
BENCHLOCK_PATH=/Users/em/src/digi/digitakt2/out/.benchlock \
DIGI_STORAGE_FAULT_ISR_SYX="$PWD/firmware/Digitakt_II_OS1.17.syx" \
DIGI_STORAGE_FAULT_ISR_DN2_SYX="$PWD/firmware/Digitone_II_OS1.12.syx" \
uv run python -m tools.devtools.benchlock run --shared \
  --label storage-pair-firmware-tests -- rustup run 1.98.1 cargo test --offline --locked \
  -p patchkit --test storage_fault_isr \
  --target-dir out/native/target-wt/digikit-dsp-sampler/nogen/storage-fault-proof \
  > out/runs/storage-fault-proof/stage-b-pair-firmware-tests.log 2>&1
```

Each unset firmware variable skips that target with an explicit reason; a
configured missing, unreadable or wrong-hash input fails. Both variables were
explicitly configured in the successful invocation: **all three firmware tests
passed**. The exact executed command/environment overrides, fixture/input hashes,
lock, target and log paths are recorded from that invocation in ignored
`out/runs/storage-fault-proof/stage-b-pair-invocation.json`. This provenance
establishes the opt-in configuration; the log alone does not.

Logs: `out/runs/storage-fault-proof/stage-b-pair-firmware-tests.log` and
`out/runs/storage-fault-proof/stage-b-pair-tests.log`. The latter records **28
machine cases**: pair 10, isolated eSDHC 9, runner 4 and SSI 5, including source-0
CIMR, level 3/4 ordering, acknowledgement, atomic preflight and snapshot backing.
Earlier isolated-delivery logs remain
`out/runs/storage-fault-proof/stage-b-delivery-tests.log` and
`out/runs/storage-fault-proof/stage-b-firmware-tests.log`.

The earlier pair checkpoint remains **[D]** initialized-state evidence. A later
explicit `Machine::step_storage_with_forced` route now combines actual storage
lines with all INTFRC sources (including source 0 and vector 191) across all banks.
It selects higher level, then INTC0/1/2, then higher source; IMR/nonzero ICR and
live IPL apply. Force persists until guest clear. Controller EMASK, CLMASK other
than 0/15 and selected level 7 reject before mutation. It shares pair preflight,
cache invalidation and poisoned-entry behavior, ignores the Oracle force knob,
and requires explicit selection on each step. It synthesizes no timer/physical
SSI requests or host scheduling; pair-only and default routes stay unchanged.

The component run passes **28 tests** (13 INTC, 15 storage machine); the extended
higher-level bank2-over-bank0 assertion also passed its named-test rerun. Logs
and exact component invocation are under `out/runs/storage-fault-proof/storage-forced-*`.

The fourth firmware test follows guest CMD13 through the actual storage ISR,
bit-13 scheduling request and vector 205/level 1 into scheduler `0x40000410` and
RTE `0x40000458` on both exact images. Eight controlled runs cover DT2/DN2,
stock/patched and CC/CC+CTOE. One initialized waiter restores its intended
registers/SP/SR/PC and then executes an ordinary guest marker instruction.
Old-task save including original A0/frame SP, queue/semaphore state, actual force
clear, W1C, no host post and stock/patched equivalence are asserted; patched raw
CTOE stays sticky. Independent static review found no issues.

All **four named firmware tests pass**, with both variables explicitly set, using
the command above with label `storage-scheduler-firmware-tests` and log
`out/runs/storage-fault-proof/scheduler-continuation-tests.log`. The exact successful
command/environment and fixture/input hashes are recorded in
`out/runs/storage-fault-proof/scheduler-continuation-invocation.json`.

This is one controlled initialized-task continuation, not native boot, a complete
scheduler/timer model, physical DMA quiescence or full relevant fault history.
Full RSTA and native fault-history acceptance remain **failed/open**; Stage C
daemon integration is unstarted. The policy audit found Board/Time snapshot
backing lacks CompletionPolicy/TimerPolicy/construction binding, while native
boot hardcodes Oracle and checks firmware identity. Runtime execution-policy
compatibility remains unaudited; no exploited mismatch is claimed. No snapshot
format migration or daemon change is included. The coordinator's scoped
`test:changed` passed pathcheck/layers before final prose; final docs pathcheck is
coordinator-owned and the full gate remains deferred. See the
[plan](../../../../docs/plans/dsp-sdk-and-sampler.md#next-proof-private-early-dt2-storage-fault-observer)
and [scope finding](../../../../docs/findings/107-storage-interrupt-emulation-scope.md).
