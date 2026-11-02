// Bounded mono float32 sample view; numerical primitives, not a sample resource/PCM ABI.
// DK_SAMPLE_CURSOR_STEP: 5 instructions. All seven arguments are distinct integer
// R registers. INDEX/FRAC are updated; TMP/CARRY are scratch. HI = rate >> 16
// (arithmetic), LO = rate & 65535, MASK = 65535; these three are unchanged.
// FRAC is unsigned 0..65535. No float absolute position and no flag dependencies.
#define DK_SAMPLE_CURSOR_STEP(INDEX, FRAC, HI, LO, MASK, TMP, CARRY) \
    TMP = FRAC + LO; \
    CARRY = LSHIFT TMP BY -16; \
    FRAC = TMP AND MASK; \
    TMP = INDEX + HI; \
    INDEX = TMP + CARRY

// DK_SAMPLE_STOCK_GAIN: float32 bits of 0.4045, the level of the stock one-shot sample voice. The stock 6-tap
// interpolator has a DC gain of 0.40439..0.40465 per row (mean 0.4045; -7.86 dB) and applies nothing else after the voice.
// The kernels below do not apply it; scale the outputs with dsp/gain_ramp.inc (start = DK_SAMPLE_STOCK_GAIN, step 0).
#define DK_SAMPLE_STOCK_GAIN 0x3ecf1aa0

// _dk_sample_linear / dk_sample_linear. : dsp/call.inc convention, 32 outputs.
// R4 = source base, R8 = output, R12 = six integer state words, normal-word DM:
//   start, end (exclusive), index, fraction, signed Q16.16 rate, done.
// Caller initializes forward at (start,0), reverse at (end-1,0), done=0;
// an empty interval is allowed. done is authoritative: reset it to restart.
// Nonempty bounds: 0 <= start < end <= 2^30-1. Initial index may be outside
// the interval (silence/completion); require -32768 <= index <= 2^30+32767.
// Source base+index and base+end-1 must be addressable normal-word DM addresses.
// rate is any signed32 word, including zero (holds). Fraction must be 0..65535.
// Bounds/rate are immutable during a call. Source, output, state and stack must
// not overlap; source is immutable while playing. L0-L7=0 on entry, unchanged.
// MODE1 is unchanged; use the module's float32 rounding mode (no SIMD).
// For each valid index: a=src[index], b=src[min(index+1,end-1)],
// y=a+float32(fraction/65536)*(b-a), with float32 rounding after each operation.
// Finite source values are required; intermediate subtraction may still overflow.
// Then advance the split cursor exactly; crossing either bound sets done=1,
// including after output 31. Finished/empty/outside reads no source, zeros tail.
// Only index/fraction/done are written back. No playback looping, fades or antialias filter.
// Saves R3,R5-R7,R9-R11,R13-R15,I0,I1 (12 slots); call.inc clobbers otherwise.
// Instruction ceiling with DK_CALL and 3 argument loads: 32*40+100 = 1380.
// Two-call test harness measured 557..2231 instructions (ceiling 2761).
// Sequential implementation; instructions are not hardware cycles.
// Needs dsp/call.inc included first. Does not depend on DK_BLOCK.
.SECTION/PM seg_pmco;
.GLOBAL _dk_sample_linear;
.GLOBAL dk_sample_linear.;
_dk_sample_linear:
dk_sample_linear.:
    DK_ENTER(12);
    DK_SAVE(2, R3);
    DK_SAVE(3, R5);
    DK_SAVE(4, R6);
    DK_SAVE(5, R7);
    DK_SAVE(6, R9);
    DK_SAVE(7, R10);
    DK_SAVE(8, R11);
    DK_SAVE(9, R13);
    DK_SAVE(10, R14);
    DK_SAVE(11, R15);
    R0 = I0;
    DK_SAVE(12, R0);
    R0 = I1;
    DK_SAVE(13, R0);
    I1 = R12;
    I0 = R8;
    R3 = R4;
    R13 = 65535;
    R15 = 32;
    R5 = DM(0, I1);
    R6 = DM(1, I1);
    R7 = DM(2, I1);
    R9 = DM(3, I1);
    R0 = DM(4, I1);
    R14 = DM(5, I1);
    R2 = -16;
    R10 = ASHIFT R0 BY R2;
    R11 = R0 AND R13;
    COMP(R5, R6);
    IF GE JUMP _dk_sample_finish;
    COMP(R7, R5);
    IF LT JUMP _dk_sample_finish;
    COMP(R7, R6);
    IF GE JUMP _dk_sample_finish;
_dk_sample_loop:
    R0 = PASS R14;
    IF NE JUMP _dk_sample_zero;
    R0 = R3 + R7;
    I4 = R0;
    R0 = R7 + 1;
    COMP(R0, R6);
    IF LT JUMP _dk_sample_neighbor;
    R0 = R7;
_dk_sample_neighbor:
    R1 = R3 + R0;
    F0 = DM(0, I4);
    I4 = R1;
    R2 = -16;
    F4 = FLOAT R9 BY R2;
    NOP;
    NOP;
    F1 = DM(0, I4);
    F2 = F1 - F0;
    F1 = F4 * F2;
    F8 = F0 + F1;
    DK_SAMPLE_CURSOR_STEP(R7, R9, R10, R11, R13, R0, R1);
    COMP(R7, R5);
    IF LT JUMP _dk_sample_crossed;
    COMP(R7, R6);
    IF GE JUMP _dk_sample_crossed;
    JUMP _dk_sample_store;
_dk_sample_crossed:
    R14 = 1;
    JUMP _dk_sample_store;
_dk_sample_finish:
    R14 = 1;
_dk_sample_zero:
    R8 = 0;
_dk_sample_store:
    DM(I0, M6) = R8;
    R0 = R15 - 1;
    R15 = PASS R0;
    IF GT JUMP _dk_sample_loop;
    DM(2, I1) = R7;
    DM(3, I1) = R9;
    DM(5, I1) = R14;
    DK_RETADDR;
    DK_RESTORE(R3, 2);
    DK_RESTORE(R5, 3);
    DK_RESTORE(R6, 4);
    DK_RESTORE(R7, 5);
    DK_RESTORE(R9, 6);
    DK_RESTORE(R10, 7);
    DK_RESTORE(R11, 8);
    DK_RESTORE(R13, 9);
    DK_RESTORE(R14, 10);
    DK_RESTORE(R15, 11);
    DK_RESTORE(I0, 12);
    DK_LEAVE(I1, 13);
