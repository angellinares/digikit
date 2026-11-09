# Sampler capacity and replacement decisions

2026-10-29. Targets: Digitakt II OS 1.17 and Digitone II OS 1.12. **[D]** denotes local measurement or source
inspection; **[O]** denotes a proposed decision or unproved contract. No new [V] or hardware claim.

## Measured capacity

**[D]** Four serial emulator snapshots used the existing `pk_devreload` startup/reload path and
`dkupload.Uploader.modules()` with its CLI formatter. Baseline means zero dynamically loaded modules;
proof means only `test/sample-proof` v0.1.0, before sample-source acquisition. All owned daemons were stopped.
Warm build checks reported no module rebuilds; no source changes, core generation or hardware runs were needed.

All values below are bytes; largest is the largest currently allocatable block reported by the runtime.

| Current image / selection | CF arena free | CF largest | DSP area free | DSP largest |
| --- | ---: | ---: | ---: | ---: |
| DN2 1.12, no dynamic modules | 524288 | 524272 | 327680 | 327680 |
| DN2 1.12, proof only | 524032 | 523312 | 293696 | 293692 |
| DT2 1.17, no dynamic modules | 524288 | 524272 | 524288 | 524288 |
| DT2 1.17, proof only | 524032 | 523312 | 490304 | 490300 |

Both proof snapshots report CF image size **232 bytes**, DSP extent **33,984 bytes** at offset 4, assigned type 16,
zero faults and guard enabled. The CF free-total reduction is 256 bytes, including allocator rounding/header cost;
the largest-block reduction also reflects allocation placement. The DSP extent includes the 32,768-byte float32
bank plus code/state/descriptor. It is not all audio storage. The leading four bytes affect largest-block size.

The device profiles reserve a **512 KiB CF runtime arena on both devices**, and a **512 KiB DT2 / 320 KiB DN2
DSP module area**, shared by all loaded modules' code and state. These are runtime-owned pools, not measurements
of stock free RAM. The four snapshots do not establish arbitrary module coexistence or a production sample budget.
See [DT2 profile](../../devices/digitakt-ii.toml) and [DN2 profile](../../devices/digitone-ii.toml).

Artifacts remain under `out/runs/dsp-sdk-checks/capacity-discovery/`: `summary.json`, the four `*-snapshot.json`
records, `*-modules.txt` formatted reports, `*-reload.json`, `measure.log`, and the out-only orchestration script.
The snapshots retain full daemon status, module lists, ready-input paths and runtime image identifiers:

| Device | Runtime image id | Stock input SHA-256 |
| --- | --- | --- |
| DN2 1.12 | `c0e8bd18a4812349` | `7e5b8a733d4ab1fdf7e5d22757f7d8eddaa3cbf71d0858874a791844c02ceb35` |
| DT2 1.17 | `3bf7458c8463c435` | `26c22f6652625ac2cfd47f7ee970d388ed8b6427dae3563c0a6a2f2d334350d5` |

The input files are `firmware/Digitone_II_OS1.12.syx` and `firmware/Digitakt_II_OS1.17.syx`; ready inputs are
`out/ready-fix-dn2-112` and `out/ready-pk-dt2-117`. The historical DN2 DSP-core display label does not change
this current-image provenance; retain the qualification in [finding 79](79-sample-bank-proof.md).

## CF acquisition cost and actual lifetime

**[D]** The current source adapter allocates a sector-rounded complete native file plus 1,024 bytes of scratch,
then allocates decoded float32 output. A canonical N-frame native file occupies `2*N + 80` bytes. With 16-byte
arena headers and aligned payloads, the simultaneous allocation formula for a full-length source at a configured
N-frame acquisition limit is:

`align16(round512(2*N + 80) + 1024) + 16 + align16(4*N) + 16`.

At the implemented 8,192-frame limit this is **50,720 bytes**: 17,920 bytes input/scratch, 32,768 output and two
16-byte headers. Applying the same strategy to 16,384 frames would require **99,872 bytes**; that is arithmetic
for a possible extension, not an implemented or approved limit. These figures exclude other runtime allocations,
fragmentation, stack use and module storage. The input allocation uses the configured maximum even for a shorter
file, while decoded output follows actual frame count. See [source acquisition](../../modules/core/runtime/src/rs/src/sample_source.rs)
and [arena allocation](../../modules/core/runtime/src/rs/src/arena.rs).

**[C] [D]** Session pinning describes the DSP bank, not a permanent decoded CF copy. Input/scratch is freed before
source acquisition returns. The decoded CF buffer survives while transport may read it, then
[runtime sample processing](../../modules/core/runtime/src/rs/src/sample.rs) lines 160–169 takes and frees the source
when the job is no longer busy. Successful READY therefore does not retain this CF audio buffer. An ambiguous
accepted transfer must remain quarantined until terminal/quiescent; the
[job contract](../../modules/core/runtime/sdk/src/sample_job.rs) prevents `take_source()` while busy.

**[D]** The DSP bank remains pinned until restart in the current proof. Release exists for load rollback; there
is no successful live-unload/replacement path. A second load is refused. This is the ownership safety established
by [the bounded proof](79-sample-bank-proof.md), not a reusable live sample-pool contract.

## Replacement decision and next proof

**[D]** The user confirmed **approximately ten-second one-shots** and **“Stop old notes on selection.”**
Ten seconds at 48 kHz mono PCM16 requires **960,000 bytes** of audio; stereo requires 1,920,000 bytes, before
metadata or allocation overhead. This is a product target, not an implemented capacity. The current proof remains
limited to 8,192 mono frames (about 171 ms at 48 kHz).

**[O]** A single-bank replacement design can follow that choice: invalidate the old view and stop its notes;
obtain an acknowledged DSP quiescence barrier proving no consumer still reads the old bank; transfer the new
payload; transfer its descriptor; publish READY last. A consumer generation/epoch contract must prevent stale
voices or queued note-ons from reusing the old view during this sequence. Neither the barrier nor this generation
contract is implemented or proven. Publishing a new pointer or clearing READY alone cannot prove old voices have
stopped reading bytes that are about to be overwritten. Source-buffer lifetime and DSP-consumer lifetime require
separate acknowledgements. No replacement implementation is authorized by this note.

**[D]** Existing DSP-link acknowledgement proves that the receiver processed a chunk, not that every voice
reader is quiescent. The receive hook runs at the common block-handler tail; ordering against all voice renders
and freedom from preemption are not established. A READY=0 write acknowledgement is therefore not a reader
barrier. See [link bindings](../../devices/fw/dsp/link.toml),
[hook installation](../../modules/core/adapters/dsp-link/module.toml), and
[finding 31](31-dsp-link.md). The current four-word descriptor contains neither a generation nor reader
tracking, so the replacement protocol needs an explicit API/ABI design or extension.

**[O]** The coordinator now prioritizes proving a **DT2 stock DSP-pool adapter and slot ownership** before
implementing the small-bank replacement protocol. The SDK interface may use device-specific backends; its
portable contract need not imply identical backing storage. Keep the current immutable-bank proof unchanged.
The quiescence and generation requirements above still apply to eventual replacement; the small-bank handshake
is no longer the immediate implementation priority. PCM16 resident-reader numerical proof and a paired A/B remain
possible later investigations, not an adopted format or speed claim.

**[C]** [Finding 81](81-stock-sampler-dependencies.md) supersedes the immediate investigation order above:
use stock DT2 OneShot to capture selected slot → voice pointer/PCM and lifecycle transitions before designing
adapter quiescence. Derive independent shared packed-PCM16 SDK semantics with device-specific ready views;
do not copy the whole stock machine. DN2 already has working playback/source acquisition: investigate missing
larger backing/delivery against those concrete dependencies, not feasibility from scratch. No new implementation
or stereo decision follows.

## Stock capacity and backend feasibility

**[D]** Elektron's official DT2 user manual documents **400 MB per project**, approximately 72 minutes mono or
36 minutes stereo, and up to 1,016 sample slots (OS 1.03A manual, page 17, section 5.2.6). Page 68 explicitly calls
this the project's RAM. Local extracted evidence is
`out/refs/Digitakt-2-User-Manual_ENG_OS1.03A_241023/pages/p0017.txt` line 10 and `p0068.txt` line 8 in the same
folder. This is documented stock capacity from that manual version, not a measurement or ownership proof for
our DT2 1.17 adapter. The 512 KiB DT2 / 320 KiB DN2 reservations above are digikit module-pool choices, **not
hardware sample-memory limits**. The ten-second target cannot fit even raw PCM16 in either current reservation.

**[D]** Current DT2 1.17 static candidates in [finding 104](104-dsp-sdk-sampler-discovery.md) are slot table
`0x257810`, rebased DSP address `0x8422b9c8`, and page-copy routine `0x1c403c`. **[O]** Slot lifetime, reclamation,
allocation ownership and cache/coherence rules remain unproved. These candidates do not verify an allocator;
older [V] marks or observations on other images do not transfer to this adapter.

**[C]** The earlier external-read leads at `0x1c8ef7` and `0x1c8f00` are withdrawn: the independent current-image
raw-byte audit in [finding 83](83-sample-retirement-design.md) identifies a non-instruction boundary and a register move.
**[O]** There is no established free or owned sample-sized DSP allocation and no
established delivery path for such a pool. A ColdFire cave or CF module arena does not prove DSP addressability.
Do not assume the DT2 pool, stock sampler or LP0 path exists on DN2. DN2 feasibility for the ten-second target
remains unresolved.

**[O]** Current next steps are the stock-machine-led proof and dependency mapping in finding 81. The newly
recorded project release/reset and preview correction refine these earlier candidates. DN2 external ranges are
a supporting question only when tied to a missing backing/delivery dependency; address appearance alone still
proves neither available RAM nor safe ownership.

The following capacity ceilings spend the entire reserved DSP area on audio, **before code, descriptors,
alignment, other modules and safety headroom**. They are arithmetic bounds, not playable product limits.

| Next option | Raw resident ceiling DT2 / DN2 at 48 kHz mono | Required evidence / decision |
| --- | --- | --- |
| Float32 single bank | 131072 / 81920 frames (2.731 / 1.707 s) | Cannot meet the ten-second target in this reservation; retain for the current proof. Replacement contract remains unproved. |
| PCM16 resident bank | 262144 / 163840 frames (5.461 / 3.413 s) | Investigate packed signed-PCM16 reads/conversion with native numerical proof and a paired A/B before adoption. Same source precision, half the float32 storage; still below the ten-second target here, with no speed claim. |
| Newly owned pool | Unknown | Prioritize DT2 stock-pool slot/ownership proof; map DN2 external ranges separately. Neither is established as available to this SDK. |

Memory aliases name the same physical storage and add no RAM. DT2 stock sample ownership does not establish an
available pool on DN2. A larger declared bank also competes with every other loaded module.

## Existing-link streaming ceiling

**[D]** Raw 48 kHz mono PCM16 consumes **96,000 bytes/s**, before framing or recovery. Current frame geometry
leaves at most 171 payload words on DT2 and 12 on DN2 after three header words. At 1,500 frames/s this gives
`171*4*1500 = 1,026,000` bytes/s and `12*4*1500 = 72,000` bytes/s respectively. These are ideal arithmetic
ceilings, not measured throughput: acknowledgements, retries, scheduling and contention reduce useful delivery.
The existing DN2 link cannot sustain that raw stream even at its ideal ceiling. Float32 would demand more.
See device profile tail sizes and [link framing](../../modules/core/adapters/dsp-link/src/cf/dsp-link.c).
Resident loading remains distinct from streaming; no throughput or loading-latency claim follows from the
capacity snapshots or the passing playback scenarios.
