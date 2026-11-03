# Isolated DDR transport diagnostic

Experimental, DT2 OS 1.17 and DN2 OS 1.12. Patch explicitly with
`apply --modules core/dsp-ddr-proof`; it conflicts with `core/dsp-link` and is
absent from default patch sets. The acceptance test loads that prepatched image
through the existing stock session's `firmware.load` API, using prebuilt binaries.
It uses the existing CF frame-call and DSP common-tail hooks.

The host writes a dedicated 2,688-byte `dd_frame` staging array, then publishes
`dd_mail.state = 4` last. The four CF mailbox words are state, error/status,
watchdog ticks and frames sent. States are IDLE=0, BUSY=1, DONE=2, ERROR=3,
START=4. Do not touch staging while BUSY. Errors are sticky until boot. Staging
remains allocated after errors. This is a host test aid, without a runtime API.

The CF wrapper passes the original arguments to the stock sender while idle.
Initial INIT substitution requires stock command 3, data length 0x802 (DT2) or 0xa80 (DN2), reply length 0xabc and zero
masks at source halfwords 17..20 in both this and the prior ISR sample. Each
START validates and caches the complete staging header, bounds and checksum;
BUSY retries reuse that result while staging remains immutable.

A valid RETIRE can bypass quiet masks within that same valid normal-frame context.
Only its exact RETIRED ACK grants successor INIT under held notes. Exact INIT and
COMMIT ACKs grant closed-bank DATA/COMMIT and OPEN respectively. A separate exact
OPEN ACK permits duplicate OPEN, without granting DATA or INIT. Duplicate COMMIT
is also permitted while sealed. INIT/RETIRE clear the previous opening grant.
Malformed or stale ACKs never grant access. This diagnostic substitutes shared
output frames during transfer, so it mutes that output; it is not a production
live-audio loading path. Otherwise CF passes the normal frame and waits at most
900 ISR ticks. The sender synchronously builds its SPI queue from staging.

Each DSP word is transmitted low halfword first. CF halfwords are stored in the
CF CPU's big-endian order. The 672-word full frame is:

| Word | Meaning |
| --- | --- |
| 0 | Exact command 4; the stock command rejection path mutes output and reaches the common tail |
| 1 | Distinct proof magic `0xDDF4A601` |
| 2 | Nonzero exact 32-bit token |
| 3 | Sequence: INIT 0, DATA 1..375, COMMIT 376, RETIRE 377, OPEN 378 |
| 4 | Operation: INIT 1, DATA 2, COMMIT 3, RETIRE 4, OPEN 5 |
| 5 | Destination offset in words |
| 6 | Payload count, at most 640 words |
| 7 | Checksum over words 0..6 followed by payload: `s1 += w; s2 += s1; check = s1 XOR s2`, modulo 32 bits |
| 8..31 | Reserved zero, excluded from checksum |
| 32..671 | Up to 640 payload words |

Initial INIT adopts any nonzero token. Later INIT accepts only the exact
non-wrapping successor after the previous generation is RETIRED with count/fault
zero. Validation precedes token adoption or guard writes; tokens never reset,
repeat or wrap during a boot. Each accepted INIT clears READY and initializes
4 KiB canary bands at byte ranges `[0x807ff000,0x80800000)` and
`[0x808ea600,0x808eb600)` to word `0xA55A3CC3`. DATA must match the next sequence
and exact next offset. Range/checksum checks precede copying. Immediate identical
accepted loading requests ACK without rewriting; replayed INIT cannot reset a
loaded generation or change its ACK identity. Stale tokens are ignored.

COMMIT requires exactly 240,000 accepted words and seals the bank with READY and
admission still zero. Only explicit OPEN publishes READY/admission. RETIRE may
retire a complete sealed bank without opening it. To skip B, finish and seal B,
retire B, then load C; there is no cancellation. After RETIRE, duplicate OPEN
cannot reopen that generation. Control sequences are exact operation identities,
not a generic increment rule: COMMIT → OPEN → RETIRE is 376 → 378 → 377.
The destination normal-word base is `0x10200000`, the emulator's byte range
`[0x80800000,0x808ea600)` (960,000 bytes).

The acknowledgement occupies exactly record bytes `0x60..0x67`: word 0 has
magic `0xA6DD` in bits 0..15, sequence in 16..24 and status in 25..31; word 1
contains the exact token. Status 0 means accepted, 1 SEALED, 2 checksum error,
3 range error, 4 protocol error, 5 RETIRED, 6 PENDING and 7 OPEN. Only errors 2..4
are DSP poison; terminal statuses 5/6 remain eligible for drain rechecks. CF-only
errors are 100 timeout, 101 malformed staging/header/range/checksum and 102 unexpected ACK status.
CF requires exact token/sequence and operation-specific success: INIT/DATA 0,
COMMIT 1, OPEN 7, RETIRE 5;
status 6 retains BUSY/staging and returns control for bounded retries. Timeout
never authorizes backing reuse. DSP errors retain the last accepted ACK sequence;
a rejected owned ordering request can therefore end as CF timeout rather than a
matching error ACK. Both outcomes are sticky and prohibit further writes.
The private host writer serializes whole load transactions and retains rejection
even when callers catch it, so no queued C can continue after uncertain B completion. The conflict with the normal link adapter prevents competing use of the hook;
it does not establish exclusive ownership of the DDR range. Boot-stream gaps and
bounded emulator results are not a hardware reservation or coexistence proof.

Focused tests: `tests/patchkit/test_dsp_ddr_proof.py`. The coupled acceptance test
is `tests/regression/runtime/sample-ddr-transfer.test.ts`; run only after review,
with explicitly supplied prebuilt device daemons (`DIGI_EMUD_DT2` and/or
`DIGI_EMUD_DN2`) and `DIGI_PATCHKIT_BIN`. It captures the baseline,
checks the entire payload and both complete canary bands through bounded reads,
and uses fresh boots for sticky failure cases. No hardware cache/coexistence,
product sampler or general live-audio loading result follows from it. The separate
terminal case below establishes only its bounded owned-bank lifetime behavior.


## Fixed resident playback stage

The same isolated adapter declares the patched-in `DDR proof` machine and its
`DDRPROOF` page, with a Reverse knob (0..1, default forward). Its diagnostic-only
permanent type 11 on each device is reserved in
`modules/core/adapters/machines/types.toml`; defaults still do
not include the adapter. The closure adds `core/machines`, `core/params` and
`core/dsp-voice`, without the conflicting DSP-link adapter.

Note-on admits only READY=1, latches a six-word cursor covering 480,000 mono
frames at 48 kHz and uses the packed PCM16 SDK reader. The source address is
fixed inside this core diagnostic and remains immutable until exact
RETIRED or reboot. It has no public resource ABI. Reverse starts at 479999 with rate -1;
forward starts at zero with rate +1. Unready notes are silent and DONE. The
bridge preserves the adapter's I12/I13 and exact stack boundary across DK_CALL.
The descriptor uses 28 bytes of state: the six SDK cursor words followed by a
private generation token. An accepted note captures the current nonzero bank
token in the masked admission section. Render compares that cached token in the
same section before admission; a mismatch or denied callback invalidates the
voice and returns silent DONE. Render cannot adopt a newer bank token. This
protects cached voices. The selected policy stops currently playing notes on
selection; queued notes starting after new READY resolve the new bank.
Conservative instruction budgets are 2400/2600 (render/note-on including render),
without a hardware cycle claim.

The consumer unit test is `tests/patchkit/test_dsp_ddr_playback.py`, including
forward/reverse retriggers, end-of-view completion and an extra silent render
with no source reads after DONE. The coupled valid scenario verifies the entire
transport and canaries before selecting this machine, then records a complete
WAV, live registry binding, early/late/end state and known 200 Hz content. Its
normal-command counter observes low-halfword command 3 in actual receive frames
after COMMIT; its 32-bit load includes adjacent pitch, which the observer masks.
The transport gate still requires exact complete word 4 plus its distinct magic. A stock
engine workspace peek is diagnostic only; downstream reuse and FX tails do not
prove the kernel's after-DONE output.


The earlier capacity/playback checkpoint is retained at
`out/runs/ddr-proof/11723539-7f66-4fce-b663-45cd9ea3d3da` (log
`out/runs/ddr-proof/transport-playback-fixed.log`). Full payload and canaries pass,
actual type 11 dispatches one note, and 480,000 frames complete in 15,000 renders
with DONE. The 552,992-frame stereo capture is finite and preserves the known
200 Hz source in both early and late windows. Normal frame observations advance
after COMMIT and through playback. Eighteen focused checks pass. This proves the
isolated emulator capacity/playback milestone. Terminal owned-bank retirement is
now covered below; hardware, reopen/replacement, Transfer UI and production
resource APIs remain separate work.


## Terminal retirement stage

Admission starts closed and opens only at explicit OPEN after complete COMMIT. RETIRE is
operation 4, sequence 377, zero offset/count, with the exact completed bank token
and valid checksum. It clears admission/READY and closes that generation permanently. Each note-on/render checks and increments the private admitted
count before backing access; guarded release follows the last possible read.
PENDING=6 means an admitted callback remains. RETIRED=5 requires a drained count;
an identical RETIRE rechecks it idempotently. No same-generation INIT/DATA/COMMIT/OPEN can reopen or
write the retired bank; only validated successor INIT may start a new generation. Corrupt/overflowing bookkeeping fails closed.

Admission, final-read release and closure save incoming MODE1, clear only IRPTEN,
wait two conservative NOPs, perform the short operation, and restore exact MODE1
with two NOPs. NESTM is unchanged; the numerical reader executes with incoming
interrupt state restored. This proof assumes one core, serialized adapter/hook
paths and maskable bank accesses. It does not prove general firmware or NMI exclusion.
A denied callback initializes empty/DONE state; render emits zeros without calling
the source reader, covering cached, delayed, held and retriggered starts.

The observer retains first/latest raw MODE1, ASTATX, I6/I7/I13 and active markers,
plus entry/exit and tail-sees-active counts. Live sequential reads can tear:
ASTATX is not a task identity and zero observed overlap is not exclusion proof.
The active observer marker is non-atomic and separate from guarded admission.

The earlier terminal-only implementation passed all five coupled cases in `out/runs/ddr-proof/terminal-retirement-suite.log`.
The terminal artifact is `out/runs/ddr-proof/ff8f8057-0a9f-4030-95ff-83143b7ac2a5`.
It requests retirement at cursor 49664/DONE=0, receives exact RETIRED at 1,572
source-kernel calls (below natural completion), then poisons the complete 960,000-byte
bank through the existing BASE-guarded diagnostic edit API. Expected full canaries
remain intact, normal frames progress after poison, and source calls stay at 1,572
across the held trigger, an actual fresh note-on and duplicate RETIRE. DONE/control
and no-fault checks pass; stock FX tails are not asserted to be immediately silent.
The full ten-second playback case also passes with the changed diagnostic at
`out/runs/ddr-proof/6f65ad4c-3cf6-4553-8500-15eadd79bc51`.

Thirty-six focused assembled/CF checks pass, including preserved-reader/shared-DM
interleavings before first read, mid-reader and after kernel return. These exercise
PENDING/drain and no-source-read denial, without claiming physical IRQ dispatch.
Raw context MODE1 is `0x39011cf8`; asynchronously sampled tail entry/exit counts
35355/35358 cannot be compared as one coherent snapshot. No hardware, DT2 capacity,
new-generation opening or strict upstream stale-event policy is established.
See [finding 83](../../../../docs/findings/83-sample-retirement-design.md) for hashes,
provenance and remaining design work.


## Reopening implementation checkpoint

The explicit successor/seal/open protocol and ACK-derived CF grants are implemented;
the existing coupled cases now issue OPEN before playback. The additional A → B → C
case retires active A, finishes and retires sealed B without opening, then opens C
and checks a fresh note's private token, full bank/guards and distinct 400 Hz audio.
All six coupled cases pass in `out/runs/ddr-proof/generation-reopen-suite.log`;
the 202.04-second wall time is not a performance measurement. The A → B sealed
skip → C artifact is `out/runs/ddr-proof/5103437f-373f-4f40-b9f9-c2ef10a73698`.
An independent full-capture audio audit finds all 72,544 frames finite and window
RMS 0.0094734175, establishing absolute audibility as well as 400 Hz dominance.
The test now saves audio metrics before verdicts and requires finite PCM/RMS above
0.001; the focused rerun also passes in
`out/runs/ddr-proof/generation-reopen-audio-check.log`. Final stale-B
RETIRE sends 900 times then times out with C's full bank/guards and admission
unchanged, and normal frames resume. Next work is the DT2 owned-bank lifecycle
proof, then reusable SDK/runtime extraction for both devices. No public API, new
hardware pool ownership or performance claim follows.
See [finding 84](../../../../docs/findings/84-sample-generation-reopening.md).
