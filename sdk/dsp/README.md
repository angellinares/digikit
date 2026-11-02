# SDK DSP kernel library

Small SHARC+ kernels for module authors. A module includes the file it needs with `#include "dsp/<name>.inc"`; there is
no shared copy at run time, the text is assembled into each module. The module build finds `sdk/` in its include path.

Two forms:

- Step macros (this directory, now): one sample of a kernel, for a module's own block loop.
- Block routines (`call.inc`, `biquad.inc`): whole-block routines on the stock C convention.

Style: [docs/sharc/style.md](../../docs/sharc/style.md), in particular section 5a (rules from the manuals).

## Register rule for macros

Every macro names all registers it uses as arguments: inputs, outputs, temporaries and constants. The caller allocates
the registers and can see which ones the macro writes. A macro touches no other register, flag-dependent state or pointer.
Constants (0.5, 1.5, 2.0, 1/32) are passed in registers that the caller loads before the loop. The caller writes the
`;` after a macro call. A macro is several instructions separated by `;`, so a loop-end label must never be on the
first line of a macro expansion: put the label on a trailing instruction (usually the store).

The exact oscillator/noise extractions below retain inline fixed constants where the existing machine schedule
loads them. Their destination registers are explicit arguments; they do not hide any register use. Integer and
floating-point register names alias the same storage: pass the matching R aliases required by the sine macro.

Rules that apply to the loop around a macro (style.md 5a):

- A block loop has 4 or more instructions.
- Do not use a DAG register within 4 instructions of loading it.
- A multiplier input that comes from the previous instruction costs stalls (2 if both inputs, 1 if one).
- Keep data in DM.
- The stall costs are not measured in the emulator; it counts instructions.

## Macros

| File | Macro | Instructions per sample | Notes |
| --- | --- | --- | --- |
| `gain.inc` | `DK_GAIN_STEP(OUT, IN1, GN)` | 1 | y = x g |
| `gain.inc` | `DK_MIX_STEP(OUT, IA, IB, MIXV, TMP)` | 3 | y = a + m (b - a) |
| `ramp.inc` | `DK_RAMP_SETUP(STEP, CUR, TGT, INVN)` | 2 per block | step = (target - cur) / 32 |
| `ramp.inc` | `DK_RAMP_STEP(CUR, STEP)` | 1 | cur = cur + step |
| `onepole.inc` | `DK_ONEPOLE_STEP(OUT, IN1, COEF, TMP)` | 3 | y = y + a (x - y) |
| `sat.inc` | `DK_SAT_STEP(OUT, IN1, ONE, HALF, K15, TMP)` | 5 | cubic soft clip, \|y\| <= 1 |
| `recip.inc` | `DK_RECIP(OUT, IN1, TWO, TMP)` | 7 | 1 / x, 2 Newton steps |
| `recip.inc` | `DK_DIV(QUO, NUM, DEN, TWO, TMP, TMP2)` | 8 | n / d |
| `recip.inc` | `DK_RSQRT(OUT, IN1, HALF, K15, TMP, TMP2)` | 16 | 1 / sqrt(x), 3 Newton steps |
| `noise.inc` | `DK_XORSHIFT32_STEP(STATE, TMP, RIGHT)` | 6 | unsigned xorshift32; RIGHT holds -17 |
| `noise.inc` | `DK_XORSHIFT32_LOAD_STEP(STATE, TMP)` | 7 | same recurrence, retains BD's inline shift load |
| `oscillator.inc` | `DK_SINE_POLY_STEP(OUT, X, SQUARE, COEF, OUT_BITS, COEF_BITS)` | 16 | BD/PD polynomial on already folded phase, -0.25 to +0.25 |
| `sample.inc` | `DK_SAMPLE_CURSOR_STEP(INDEX, FRAC, HI, LO, MASK, TMP, CARRY)` | 5 | exact split integer/fraction update for signed Q16.16 rate |
| `sample_pcm16_fetch.inc` | `DK_SAMPLE_PCM16_FETCH_WORDS(BASE, INDEX_A, INDEX_B, WORD_A, WORD_B, ADDR, DAG)` | 8 per pair | raw packed containing words; caller-owned protection and validated scalar addressing |
| `sample_track.inc` | `DK_STRACK_CHECK_ADMISSION`, `_CHECK_EPOCH`, `_LATCH_EPOCH`, `_ACQUIRE`, `_RELEASE`, `_FAIL_CLOSED`, `_POISON_COUNT` | as `sample_bank.inc` | the bank macros for one of the 16 track records of `<dk/sample_track.h>`: the record and the control set are addressed through two I registers instead of fixed symbols |

Each file has a header with the maths, the arguments, the instruction count and the accuracy. The float32 reference
models are in `tools/patchkit/dsp_ref.py`; the tests are in `tests/patchkit/test_dsp_kernels.py`.

The extracted noise/sine references and tests are `tools/patchkit/dsp_osc_ref.py` and
`tests/patchkit/test_dsp_osc_kernels.py`. Noise seeds, state storage and float conversion remain caller-owned;
zero is an absorbing state. The sine macro approximates `-sin(2*pi*x)` on its folded input domain, with separate
float32 rounding at each operation; phase wrapping, folding and modulation remain in the machine. These macros
replace the corresponding code in BD, PD and pluck while preserving the assembled instructions and data.

## Speed

Sequential macros do not pack stages into multifunction instructions (a multiply and an add in one instruction, or a
load with a compute). The counts above are the sequential counts. For peak speed a module interleaves the stages by
hand; the block routines (below) do this inside.

## Block routines

A block routine processes one block of `DK_BLOCK` (32) samples and uses the stock / selcc C convention, so assembly modules
and `selcc`-compiled C call it the same way. `call.inc` has the macros and the full contract; the points that matter:

- Arguments R4, R8, R12; no result. Normal-word DM addresses (a C caller passes them as `int`; selcc's byte-address pointers do
  not match). The caller sets up M5 = 0, M6 = 1, M7 = -1, M14 = 1 as the module contract already guarantees.
- A caller assumes R0, R1, R2, R4, R8, R12, I4, I12, MR and ASTAT are clobbered. R3, R5-R7, R9-R11, R13-R15, I0-I3, I5, M1-M3
  and B3 are kept. I6 and I7 are restored by the routine's `RFRAME`.
- Stack: `DK_CALL(fn, return_label)` (`CJUMP` plus two pushes) builds the frame, `DK_ENTER(N)` reserves N save slots at `[I6 - 2]` downward, and
  `DK_LEAVE` returns and runs `RFRAME`.
- Every call supplies a continuation label unique within the assembled object, including repeated calls to the same routine.
  The macro defines that label and pushes its relocated address minus one through an immediate Type16a store.
  The former one-argument form is unsupported; storing the PC UREG produced an invalid manual return frame.
- Cost of a call with S saved registers: 2 S + 7 instructions on top of the work.
- Symbols: `_dk_<name>` for assembly, `dk_<name>.` for selcc C. Include `dsp/call.inc` once, then the routine files.

`DK_CALL` uses the CJUMP ABI; the machine adapter enters a module with JUMP and a different return contract.
A module calling a block routine must preserve its adapter-return registers I12/I13 and restore its exact entry I7.
Entry I7 is an exclusive stack boundary: reserve space below it before constructing the CJUMP frame, so no store
overwrites the adapter's context. The [sample-proof fixture](../../tests/fixtures/modules/sample-proof/README.md)
contains a tested bridge for both adapters; this fixture does not define a general module-call API.

| File | Routine | Instructions per sample | Per call (32 samples, with `DK_CALL`) | Notes |
| --- | --- | --- | --- | --- |
| `biquad.inc` | `_dk_biquad` | 5 | 203 | transposed direct form II; R12 = 7 words b0, b1, b2, a1, a2, s1, s2; in place allowed; reads only the input block |
| `delay.inc` | `_dk_delay` | 4 | 175 | linearly interpolated delay, hardware circular ring; R12 = 4 words ring base, ring length L (>= 34), write offset w, delay d (float, 1 <= d <= L - 2); in place allowed; L1 and L2 are 0 on return |
| `halfband.inc` | `_dk_halfband_down` | 20 per output (2 inputs) | 701 | 2x decimator, 23-tap half-band (Kaiser window), 64 inputs to 32 outputs; R12 = 2 words history ring base (32 words), offset (unchanged by a call); coefficient table 2 c_j in DM (`.SECTION/DM`); L1 is 0 on return |
| `gain.inc` | `_dk_gain_ramp_stereo` | 7 | ceiling 254 including three argument loads | in-place gain ramp on two 32-float buffers; R12 = pair [start, step]; writes the reached gain back to pair[0] |
| `sample.inc` | `_dk_sample_linear` | path-dependent | ceiling 1,380 including three argument loads | bounded mono float32 view; split cursor, clamped linear interpolation, zero tail and completion |
| `sample_pcm16.inc` | `_dk_sample_pcm16_linear` | path-dependent | ceiling 2,148 including three argument loads | packed signed PCM16 view; same cursor, interpolation and completion |
| `sample_pcm16_stereo.inc` | `_dk_sample_pcm16_stereo_linear` | path-dependent | ceiling 4,396 including three argument loads | two packed PCM16 planes, one shared cursor, separate 32-float outputs |

For the older block kernels, consult their headers and `tests/patchkit/test_dsp_kernels.py` for the distinction
between static instruction counts and measured counts. The sample reader's measured evidence is described below.

## Numerical sample reader

`sample.inc` provides numerical playback only: it does not load files, allocate sample memory or define a sample
resource ABI. Include `dsp/call.inc` first. Pass normal-word DM addresses in R4 (source), R8 (32-float output) and
R12 (six integer state words): `start`, exclusive `end`, `index`, `fraction`, signed Q16.16 `rate`, `done`.
Initialize forward playback at `(start, 0)`, reverse at `(end - 1, 0)`, and clear `done`. A zero rate holds position.

The source, state, output and stack must not overlap. The source view is immutable and its entire interval must
be mapped/addressable; see the include for integer range and register preconditions. The reader interpolates
`a + fraction/65536 * (b - a)` with separate float32 rounding and clamps the neighbor to `end - 1`. An empty or
finished interval reads no source and writes silence. Completion is recorded even when it occurs on the final
sample of a block; subsequent calls stay silent until the caller resets the state. Only index, fraction and done
are written back. The cursor uses integer arithmetic, so large frame positions do not lose fractional precision.

The reference and tests are `tools/patchkit/dsp_sample_ref.py` and `tests/patchkit/test_dsp_sample_kernels.py`.
Tests compare against an independent combined-position oracle and watch for out-of-interval reads. The two-call
harness measured 557–2,231 instructions across its cases, below its 2,761-instruction ceiling. These are concrete-core
instruction counts, not hardware cycles or a speedup claim. This first implementation is sequential and has no
anti-alias filter, playback looping or boundary fades. Float32 input is a numerical view, not a decision to store
all transferred PCM as float32. Resource ownership and stereo playback remain separate work.


`sample_pcm16.inc` adds `_dk_sample_pcm16_linear` (`dk_sample_pcm16_linear.` for C).
Include `dsp/call.inc`, then `dsp/sample.inc` for its shared cursor macro, then
`dsp/sample_pcm16.inc`. It uses the same six integer state words and calling
convention. R4 addresses immutable packed normal-word DM: frame `i` is in
`base + floor(i/2)`, with even frames in the low 16 bits and odd frames in the
high 16 bits. Little-endian transport bytes store each frame's low byte first.
The backing storage has `ceil(total_frames/2)` full words; an odd frame count
still requires the final word's unused high half. Odd starts and ends need only
the containing words; the unused halves are ignored.

The selected half is sign-extended and normalized exactly as `int16/32768` in
float32, then interpolated in the same operation order as the float reader.
The neighbor clamps to `end - 1`; no containing word beyond the view is read.
Empty, completed and out-of-bounds cursors read no source and emit silence.
The caller handles resource readiness. The source base is a normal-word address,
without a firmware address, allocation policy or physical-memory mapping in this
contract. The numerical tests exercise packed transport bytes, signed extrema,
odd boundaries, extreme rates, repeated calls, preserved registers and stack
boundaries, guarded source reads and completion immediately after output 31.
The ceiling above counts instructions, not hardware cycles.

`sample_pcm16_stereo.inc`, included after the mono reader, composes that reader
for two channel planes. R4 points to `[left_source, right_source]`, R8 to
`[left_output, right_output]`, and R12 to the same six-word cursor. Each output
contains 32 floats. Both channels start from the same cursor; the public cursor
advances once. Source pointers may be equal for mono-to-stereo playback. Output
buffers must be separate; all other source, output, descriptor, cursor and stack
ranges must be disjoint and the sources/descriptors immutable during the call.
The wrapper uses ten local stack words; with its nested mono call, the deepest
write is word slot 25 below entry I6 and the deepest stack pointer is slot 26. The adapters give a
byte-alias I6 (a word is 4 bytes) and the wrapper finds its cursor copy at I6 − 44 bytes; a normal-word
I6 is not supported (finding 111). The calling convention
and mono reader's numerical bounds still apply. This is a correctness baseline,
not a measured optimization or a stereo machine-output ABI. Assembled coverage is
in `tests/patchkit/test_dsp_sample_stereo_kernels.py`; actual memory ownership and
stereo routing through each device's stock processing remain adapter obligations.

## Block gain ramp and stock level

The sample kernels apply no gain and no fades. `gain.inc` provides `_dk_gain_ramp_stereo` (`dk_gain_ramp_stereo.` for C)
for that: R4 and R8 address the left and right buffers (32 floats each), R12 addresses the pair `[start, step]`. Each
sample of both buffers is multiplied by a running gain that starts at `start` and decreases by `step` after every sample.
Step 0 is a constant gain; `start` to 0 is an end fade. The routine writes the gain it reached (`start - 32 * step`) back
to `pair[0]`, so the next block can continue the ramp. It does not clamp; the caller chooses the step. It needs only
`dsp/call.inc`. `sample.inc` defines `DK_SAMPLE_STOCK_GAIN` (float32 bits of 0.4045, the DC gain of the stock one-shot
interpolator, -7.86 dB); use it as `start` to match the stock level.

## Raw packed PCM16 fetch

`sample_pcm16_fetch.inc` exposes only a caller-owned raw-word primitive. It reads
`BASE+floor(INDEX_A/2)` then `BASE+floor(INDEX_B/2)` as two full NW32 words, without
selecting halves or converting/interpolating samples. Raw normal-word addressing
is the SHARC backend implementation behind this SDK primitive; it adds no resource,
output-pointer or transport ABI.

The caller validates nonnegative frame indices, bounds and any clamped neighbor.
Both containing-word addresses must be valid and nonwrapping within one address
space; even an odd terminal frame needs its full containing word addressable.
Use SISD with PEYEN clear and a caller-owned linear DAG (L register zero). All
Dreg operands are distinct except that the two input indices may name the same
register. BASE/indices are preserved; outputs, address scratch, DAG and integer
condition flags are clobbered. An include guard permits repeated inclusion and
expansion, with no labels, storage or memory writes.

For example, after validating R7 and clamping its neighbor into R12, with R4
holding the source base and protection already held:

```asm
#include "dsp/sample_pcm16_fetch.inc"
DK_SAMPLE_PCM16_FETCH_WORDS(R4, R7, R12, R8, R0, R1, I4);
// R8/R0 now hold the two raw packed words; keep/copy them before clobbering.
```

The caller must own/protect both containing words throughout both reads. The
macro provides no validation, locking, atomicity, admission, allocation, lifetime
or cache policy. It is eight instructions, not a cycle/performance claim; immediate
DAG write/use is hardware-interlocked (public SHARC+ PRM Rev 1.5, printed4-63/64),
so hardware stalls are separate from model instruction counts. This qualifies the
general DAG scheduling advice above without inserting NOPs into the primitive.

The [private frame-fetch proof](../../docs/findings/99-stereo-frame-fetch-exclusion.md)
uses two expansions inside one admitted masked fetch for both stereo planes, then
releases backing and computes from private copies. Its exclusion and callback
storage ownership policy remain private experimental obligations. Existing mono
and stereo block kernels are unchanged.

## Sample-bank lifecycle

`sample_bank.inc` supplies small inline lifecycle operations with no storage,
labels or externs. Every state address, state offset, R/I register and branch
target is a caller operand; the caller retains control-flow order. Its include
guard permits multiple consumers in one translation unit. It defines no sample
capacity, backing layout, allocation, firmware address or transfer protocol.
The numerical sample readers and their six-word public state remain unchanged.

| Operation (`DK_SAMPLE_BANK_` prefix) | Effect | Instructions |
| --- | --- | --- |
| `LOCK`, `UNLOCK` | Save exact MODE1, clear IRPTEN, restore exact MODE1; two conservative NOPs after each change | 4, 3 |
| `CHECK_ADMISSION` | Reject closed admission; fault on negative or saturated reader count | 12 |
| `CHECK_EPOCH` | Reject zero or mismatched cached generation | 6 |
| `LATCH_EPOCH` | Require READY exactly one and nonzero generation; store a fresh voice's generation | 8 |
| `ACQUIRE` | Increment a validated reader count | 3 |
| `RELEASE` | Decrement a positive count, or branch to caller's fault handler | 6 |
| `FAIL_CLOSED`, `POISON_COUNT` | Set sticky fault/closed, clear open/READY; prevent a corrupt release from claiming drain | 6, 2 |
| `CLOSE` | Clear open/READY, set closed | 5 |
| `CHECK_DRAIN` | Branch if any reader remains or a fault is recorded | 4 |
| `PUBLISH` | Set open/READY after caller's generation initialization | 3 |

Every check and update requires the caller's local interrupt mask held. Keep
admission check, epoch check/latch and acquire in one short masked scope. Restore
incoming MODE1 on every success, denied and fault branch; no backing read is
allowed before acquire. Acquire protects immutable backing
until a separately masked release after the final possible read. A failed release
must fail closed and poison the count; faults never authorize backing reuse.
CLOSE followed by a successful exact drain permits the serialized producer to
reuse backing. Initialize the next generation, including its closed flag, before
publication, with closed initialized to zero only for that successor, fault zero
and a nonzero token. The caller validates the successor and never reopens a
retired generation. PUBLISH itself does not reset closed or erase faults.
CHECK_ADMISSION does not read the sticky fault: callers must maintain the
invariant that any fault implies closed is nonzero and open/READY are zero.

Only a fresh note may latch the current nonzero generation. Cached voices compare
their saved generation and cannot adopt a replacement. Token succession,
operation validation, stop-old-notes policy and the source snapshot lifetime are
runtime responsibilities. The existing runtime `sample_job::Job` retains its
source through transfer completion and publishes descriptor READY last; these
macros do not change that ownership or the public float32 ABI.

All named R temporaries and arithmetic flags are clobbered; state I registers are
read only and must already satisfy DAG latency. Distinct register operands are
required within each macro, counting R/F aliases as the same register. Control
words and cached epoch storage must not overlap one another or backing/output.
The saved MODE1 register must stay live, distinct
from every register mutated throughout the masked region, and readable in the
incoming selected register bank: do not switch banks inside that scope. LOCK and
UNLOCK change no arithmetic flags. See the include header for exact operands and
preconditions; caller supplies each trailing semicolon and branch label.

The contract assumes one core, maskable participants and one serialized producer.
Local IRPTEN masking establishes neither universal firmware/NMI exclusion nor
hardware memory ownership, cache coherence or a production allocator. The counts
above describe instructions, not hardware cycles. The standalone assembled fixture
in `tests/patchkit/test_dsp_sample_bank.py` shows complete admission, denied,
fault, release and producer paths with two independently bound banks.
