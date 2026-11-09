# Shared sample-bank lifecycle primitives

2026-10-29. Targets: DT2 OS 1.17 and DN2 OS 1.12. **[D]** denotes source or
measured evidence; **[O]** an outstanding obligation. This extracts the proven
[DT2/DN2 lifecycle](85-dt2-sample-lifecycle.md), tracked in the
[sampler plan](../plans/dsp-sdk-and-sampler.md).

## Extracted boundary

**[D]** `sdk/dsp/sample_bank.inc` provides inline primitives for exact incoming
MODE1 save/mask/restore, admission validation, cached-generation comparison,
fresh-generation capture, reader acquire/release, fail-closed bookkeeping,
closure, drain checking and publication. Both producer and consumer in
`modules/core/adapters/dsp-ddr-proof/src/dsp/` now use these operations. The
include declares no storage, labels, firmware address, bank capacity, wire
operation or public ABI. Each control word, scratch register, state pointer,
state offset and branch target is supplied by the caller.

**[D]** The contract preserves the original short masked sections and two
conservative NOPs after mode changes. All lifecycle checks/updates require the
caller's mask held. An advisory unmasked drain check in the diagnostic header
remains literal; the authoritative check is repeated under the mask. Admission,
generation validation and increment share one masked scope. The reader runs
with incoming MODE1 restored; release occurs after its last possible backing
read. Every denial/fault branch must restore the saved mode. The saved register
must remain live, distinct from modified scratch and in its original bank.

**[D]** The caller maintains sticky fault implying closed admission/READY=0;
CHECK_ADMISSION intentionally does not add a fault read to the proven schedule.
A corrupt release must fail closed and poison the count. A serialized producer
must validate a nonzero exact successor and a completed drain before reuse,
initialize its new generation's closed flag, then publish only a fully immutable
bank. Publication does not clear faults or validate protocol identity. Cached
voices compare their saved token; only a fresh note may latch the current token.
The SDK README and include document non-overlap, register/flag clobbers and
caller control-flow obligations. These primitives are not a standalone safe
allocator, inter-core lock, NMI barrier or hardware cache-coherence guarantee.

## Validation and exact identity

**[D]** Worker verification passes 99 focused assembled tests: the existing 84
in `tests/patchkit/test_dsp_ddr_proof.py` and
`tests/patchkit/test_dsp_ddr_playback.py`, plus 15 new cases in
`tests/patchkit/test_dsp_sample_bank.py`. The new fixture binds two independent
banks to different symbols, scratch registers, state pointers and labels. It
checks fresh/cached admission, denied epochs, multiple-reader count/drain behavior,
corrupt counts/releases, sticky faults, new-generation stale-voice rejection,
exact incoming MODE1 restoration and no cross-bank writes. Existing tests retain
actual-reader interleavings and protocol failure coverage. Python lint/format
and diff checks pass. Independent review found no qualifying issue.

**[D]** Parent comparison against `e5c274d6` confirms every DSP object field
except `source_sha256` is identical: code/data sections, symbols, relocations,
architecture/format and toolchain metadata. It still re-decodes 912 instructions;
CF is unchanged. Reapplying the module with the existing prebuilt patchkit
produces the exact previously passing complete firmware bytes:

| Image | Complete patched SYX SHA-256 |
| --- | --- |
| DT2 1.17 | `ea91e7f75e249c9381e1d8028f8fa85c21009700fffd28b448e9e5fb0adf2424` |
| DN2 1.12 | `63660603260b0dc3af307577e7bb223aecc8e8f72342f8e95caf0ddeb98e3508` |

Artifacts are under `out/runs/sample-bank-extraction/`, including
`image-identity.json`, both plans/images and the original DSP object snapshot.
No firmware-derived artifacts are committed. The coupled results in finding 85
apply to these identical images; no emulator or generated-core build/rerun was
performed. Exact instruction identity preserves the schedule and golden bytes;
it is not a cycle-count or hardware speed claim.

## Runtime integration boundary

**[D]** `modules/core/runtime/sdk/src/sample_job.rs` already supplies a shared
upload job that retains a source until the link is terminal and publishes the
existing descriptor READY last. `modules/core/runtime/src/rs/src/sample_source.rs`
already snapshots and validates sources; `sample.rs` serializes jobs. These
implementations are unchanged. The public contract in `sdk/include/dk/sample.h`
and `sdk/sdk.toml` remains one pinned module-owned mono float32 bank, up to 8,192
frames, with no replacement after delivery begins. It cannot silently acquire
the diagnostic's packed-PCM16 generation protocol or larger backing.

**[O]** Product integration needs an explicitly owned larger bank, a packed-PCM16
source/transport path, and a serialized runtime selection state machine that
closes admission, drains, retires, loads/seals and opens the next generation.
Reuse existing source snapshot/lifetime machinery where applicable. Selection
stops old voices; queued notes beginning after new READY use the new bank.
The private hardcoded emulator range is not a production allocation API.
Hardware ownership/cache/coexistence and uncertainty after failed transfer remain
separate obligations. No public ABI/version change was made in this extraction.

**[D]** Direct host checks pass 195 with 16 skipped
(`out/runs/sample-bank-extraction/host.log`). Scoped changed-document and new-test
path checks pass. Routed changed checks pass Ruff and layercheck, but retain the
baseline ten stale references in five unchanged files and the pnpm launcher
failure against the linked dependency directory. The initial path count also
included new files before staging and a slash-separated comment that was corrected;
these were resolved before commit. Logs: `out/runs/sample-bank-extraction/changed.log`
and `pathcheck.log`. No clean routed integration gate is claimed.
