# DT2 sample lifecycle port

2026-10-29. Targets: DT2 OS 1.17 and DN2 OS 1.12. **[D]** denotes a read or
measured observation; **[O]** an outstanding obligation. Independent evidence
agents checked the decision-critical static claims against original image bytes.
This extends the [generation reopening proof](84-sample-generation-reopening.md)
and is tracked in the [sampler plan](../plans/dsp-sdk-and-sampler.md).

## Current-image transport audit

**[D]** Original DT2 SYX SHA-256:
`26c22f6652625ac2cfd47f7ee970d388ed8b6427dae3563c0a6a2f2d334350d5`.
Freshly extracted section 7 is 321,016 bytes, SHA-256
`0f514a12a2255f5c081e292c47f1f29462003177658da4bbae0a22fd737fffa2`,
matching the cached section and database metadata.

**[D]** CF calls at `0x4002dd60..0x4002dd74` pass record `0x8000488c`,
reply length `0xabc`, source `0x80005348`, and data length `0x802` to
`0x400cd420`. Initialization obtains `0xabc` from `0x4002f472` and stores it
through `0x400cd414` at `0x42948b08`. The driver caps the physical transfer at
`0xbc0`; the unchanged diagnostic data length `0xa80` fits the configured frame.
Its 672 words retain the existing 32-word header and 640-word payload. Four event
masks occupy source halfwords 17–20; index 21 is another control halfword whose
full semantics are not established. Quiet masks are admission observations, not
a queue-drain or inactive-reader proof.

**[D]** The stock command load at SHARC `0x1c750a` is unsigned SHORT16.
Command 4 rejects before table lookup and enters `0x1c7524`, muting output before
the common tail. The table at loader byte `0x2825f7b0` contains targets
`0x1c7524`, `0x1c75d8`, `0x1c763c`, `0x1c7671`. The rendered indirect jump
uses misleading DAG1 names; raw fields select DAG2 I12/M13. Receive context is
`DM(I6-3)`; the tail reloads reply I4 from `DM(I6-2)` at `0x1c75b1`, before
hook `0x1c75c1`. The existing two normal-word ACK stores occupy exactly eight
bytes, offsets `0x60..0x67`. No table expansion or DSP protocol change is needed.

## Candidate bank and limits

**[D]** Original section 7 contains 104 boot blocks. Block 101 ends at byte
`0x8055e890`; block 102 fills `[0x80a00000,0x82a001c4)`. The full candidate
and guards `[0x807ff000,0x808eb600)` lie in the intervening gap and have no boot
block owner. The payload `[0x80800000,0x808ea600)` holds 960,000 bytes: ten
seconds of 48 kHz mono PCM16. Normal-word payload base is `0x10200000`.

**[D]** Original DT2 instructions at `0x1c4053`, `0x1c3fc6` and `0x1c3fcb`
contain stock sample base `0x8422b9c8`. The page routine accepts page indices
below `0x19200` and computes base plus page shifted left 12. The CF allocator's
`0x19000000` offset limit gives nominal end `0x9d22b9c8`; the DSP page bound
instead gives `0x9d42b9c8`. Neither calculation is an ownership proof. The
candidate and guards are below this stock mapping.

**[D]** Independently checked original DT2 init block 2 contains DMC value
`0x0af00622` at loader byte `0x28242c4c`. Original init code passes `0x242c4c`
at short-word `0x120520`, calls `0x120353`, loads through I4 at `0x120360`, then
writes register `0x31070040` at `0x120362`. Inspect original block payloads:
subsequent boot loading overwrites this init code. The public HWR register map
identifies DMC0_CFG and Table 9-6 interprets its fields as one external bank,
16-bit width and 4-Gbit SDRAM, or 512 MiB configured geometry. Reference:
`out/refs/adsp-2156x-hwr/all.txt`, register map and printed pages 9–17/18.
Finding 83's `0x8052fbe0` loaded end is DN2 evidence, not a DT2 bound.

**[O]** Runtime users, later firmware writes, cache behavior, invalid-metadata
reads and hardware reservation remain unproved. The decision is a bounded
emulator experiment in this candidate range. Conflict with `core/dsp-link`
reserves a hook, not the memory. This does not authorize borrowed stock sample
backing or a public sample allocator.

## Port and validation

**[D]** The private module supports DT2 1.17 and DN2 1.12 explicitly and remains
absent from default module sets. CF admits exactly normal data lengths `0x802`
and `0xa80`, retaining the same reply length, full diagnostic capacity and
lifecycle protocol. DSP source/object and SDK cursor/reader remain unchanged.
DT2 and DN2 both use diagnostic type 11. Historical DT2 type 10 was assigned to
`test/twin-xslice:xslice_d` in commit `d1f1e4d1`, then removed in `1eb92a86`;
it is now explicitly marked retired and is not reused.

**[D]** Eighty-four focused assembled CF/DSP tests pass. Both supported frame
lengths cover admission/ACK/watchdog and held-note successor grants; unsupported
lengths preserve original arguments without substitution. Scoped lint, format,
regression TypeScript and object freshness checks pass. Rebuilt CF object
re-decodes 372 instructions; DSP is unchanged.

**[D]** Independent review caught a device-specific test observation: DT2
`_dv_c` word 0 contains the note-on flag, while DN2 uses it for machine type.
The corrected proof reads DT2 `_dv_g + 4`, retaining the live registry assertion.

**[D]** Initial coupled suite in `out/runs/ddr-proof/dt2-port-suite.log` passed
nine cases and failed three DT2 cases during playback setup. The corrected rerun
is recorded below. No hardware, performance or clean integration-gate claim is made.

**[D]** Initial DT2 transport completed all 960,000 bytes with both initialized 4,096-byte
canary bands matching completely and READY=1. Payload SHA-256:
`d4fa333d949c55fd4c9751ab5a5ca852ceba2a6835533898096dc66bddde8a82`.
Artifact: `out/runs/ddr-proof/dt2/ddee7312-1aa3-44bc-adc0-bbcc85a30c94`.
Playback then stopped at a test assumption: registry `position=255` means append,
not actual menu row 255. Both machine adapters clamp that insertion request when
building `mf_list_buf`. The test now reads the live list after menu entry, verifies
its exact membership and unique diagnostic type, and selects the observed row.
List length is stock count plus registered entries, excluding retired gap rows.
Independent review confirms this correction and the dispatch observation change.

**[D]** Host checks pass 195 with 16 skipped
(`out/runs/ddr-proof/dt2-port-host.log`). Scoped changed-document pathcheck passes.
Routed changed checks retain the existing ten stale references in five unchanged
files and the pnpm launcher failure against the linked dependency directory;
Ruff and layercheck pass. Log: `out/runs/ddr-proof/dt2-port-changed.log`.
Direct lint, format and regression TypeScript checks pass for the changed test.
This is not a clean routed integration gate.

**[D]** Independent artifact audit regenerated the fixture and baseline hashes.
All 968,192 baseline bytes (candidate plus guards) were unknown before INIT;
unknown does not mean unused or zero. The 379 completed requests comprise INIT,
375 DATA packets plus the first-packet duplicate, COMMIT and OPEN. No standalone
post-transfer bank dump is retained: exact equality is supported by the test
reaching its result write after full readback assertions. The saved patched hash
is `ea91e7f75e249c9381e1d8028f8fa85c21009700fffd28b448e9e5fb0adf2424`.
Live status identifies DT2 1.17 firmware and generated DSP core `dt2-1.16`;
this is the existing prebuilt emulator, not a new core regeneration.

## Corrected full-playback evidence

**[D]** DT2 full transport and ten-second playback pass at
`out/runs/ddr-proof/dt2/a3273fdd-d212-4cb8-98ce-025ed8d6c427`.
The live menu contains `[0,1,2,3,6,4,5,11]`, placing diagnostic type 11 at row 7;
its live dispatched type is 11. Final cursor is
`[0,480000,480000,0,65536,1]`, with 15,000 blocks, one note and DONE=1.
An independent audit checked all 339 saved snapshots: cursor and block counts
are nondecreasing and include intermediate and late active positions.

**[D]** The independent audit decoded all 552,544 captured stereo float32 frames
at 48 kHz; every sample is finite. Both windows (seconds 1–2 and 8–9) reproduce
RMS `0.02293529305352567`, 240-frame correlation `1.0`, and 120-frame correlation
`-0.999999999998114`, confirming expected 200 Hz content. WAV SHA-256:
`7210398cef5e6d8e72a057e2142b770643eaeaa46a31540365b807f981632f7c`.
The pass marker follows the no-fault assertion; no standalone final fault dump
is retained. This case does not establish post-completion silence, universal
callback exclusion, retirement/reopening or hardware behavior.

## Corrected terminal retirement evidence

**[D]** DT2 terminal retirement passes at
`out/runs/ddr-proof/dt2/4f054e94-6ff7-4483-91b3-22164872ea6f`.
An independent audit confirms the request occurred at cursor 48,928 of 480,000,
DONE=0. Exact token `0x81234567`, sequence 377, operation 4 completes with CF
state 2 and status 5 before poison injection. Retirement and duplicate retirement
both complete with that identity. Four later observations report READY/open=0,
closed=1, admitted/fault=0 and unchanged read count 1,549. The fresh trigger raises
note callback entries from 1 to 2 without further source reads.

**[D]** The retained patch independently decodes to 960,000 bytes of `0x5a`
at `0x80800000`; its hash matches recorded post-injection readback:
`af99c4a7ac4ce10d285f03424fa2f0757c8c7e12306b61f5b63964c2a0caec06`.
Each complete initialized guard retains hash
`539a03c496fbb7fca763db383a58ecb3373356a98a31b79fd5421277a9652cd5`.
No standalone post-poison dump or raw DSP ACK dump is retained; sequencing rests
on executed assertions and request records. The bounded counters do not establish
global callback exclusion, upstream event age or downstream effect-tail silence.

## Reopening and completed validation

**[D]** All six affected playback/lifecycle cases pass on both devices in
`out/runs/ddr-proof/dt2-port-playback-corrected.log` (six passes, six rejection
cases filtered out). Its 443.42-second wall time is not a performance measurement.
The initial suite already passed the three rejection cases on each device;
together these runs cover all six cases per device without repeating unchanged
rejection checks. DN2 corrected artifacts are:

- Full playback: `out/runs/ddr-proof/dn2/1e08fcc2-27f2-436f-8678-9d83ddb02bbc`.
- Retirement: `out/runs/ddr-proof/dn2/06931d08-401f-4d04-9b70-971525556a4f`.
- Reopening: `out/runs/ddr-proof/dn2/93dea216-9a80-41f3-99fc-f6482f1dfc34`.

**[D]** DT2 reopening passes at
`out/runs/ddr-proof/dt2/efe92f48-b20f-4b3c-9526-fa462c245b34`.
B token `0x81234568` is sealed with READY=0, matching full-bank hash
`49dd22e26896cae821006bf469838eca5d6ddde1d76818c10e34ca14eb395dc8`.
C token `0x81234569` opens only after B retirement. Its source and complete
readback both hash to
`7c942f58087983416e16862cc3141a55fa43e27bbd93ffe44d25fb80fc49b448`;
both initialized guards retain the expected canary hash. A fresh note latches C
and produces 400 Hz audio. The entire 73,984-frame capture is finite; measured
window RMS is `0.022944933991606277`, with 400 Hz power `149417.27106810006`
and negligible 200/300 Hz power.

**[D]** A stale-B retirement after C opens times out with CF error 100 after 900
sends. C token, READY/open=1 and full bank plus guards remain unchanged; normal
frames advance from 62,977 to 62,991. Full bank-and-guards hash:
`e8fd8e7a5e03ca1458bc57b7460c856273b73dcdda3d35900ac2a14c46ede93d`.
This is the same private lifecycle contract as DN2, not a public production API.

**[D]** Independent final audit confirms A was active at cursor 49,472/DONE=0
before retirement, and source reads remained 1,566 through B's skipped generation
until C's fresh note. The request log has B COMMIT then RETIRE, with no B OPEN,
and an explicit C OPEN. Independently regenerated B/C payload and combined
C-plus-canary hashes match the recorded results. Independent C WAV decoding
reproduces RMS `0.022944933991605864` and 400 Hz power `149417.27106810716`;
small differences from recorded sums are floating-point rounding. WAV SHA-256:
`1550ac6a6a027851eae50aff03ece5b8a9803538dfa015183403cc3a0a5ec9ef`.
C's capture is approximately 1.54 seconds, not a second full-duration playback
proof. Raw post-transfer and ACK dumps remain absent; no global exclusion,
upstream event-age, hardware or performance conclusion follows.
