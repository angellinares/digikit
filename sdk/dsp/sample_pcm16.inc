// Packed signed PCM16 numerical reader; no sample resource ABI or physical-memory mapping.
// Include dsp/call.inc and dsp/sample.inc first (DK_SAMPLE_CURSOR_STEP dependency).
// _dk_sample_pcm16_linear / dk_sample_pcm16_linear.: same six-word state, 32 outputs,
// bounds, cursor, completion, rounding, register and disjoint-memory contracts as
// _dk_sample_linear. R4 is the normal-word DM source base; R8 output, R12 state.
// Frame i occupies word base + floor(i/2), even frames in low 16 bits, odd in high.
// Little-endian transport bytes pack as low byte, high byte of each signed frame.
// Storage contains ceil(total_frames/2) full words, including an unused high half
// for an odd total. The containing words for [start,end) must be addressable;
// halves outside the interval are ignored. Source words remain immutable.
// Each selected half is sign-extended and converted exactly to float32(int16/32768).
// Interpolation clamps its neighbor to end-1, then uses the float reader's order.
// Loop option: the caller sets bit 1 of state word 5 (done) before the call. The
// kernel reads only bit 0 as done, and writes word 5 back as 0 or 1 (the loop bit
// is an input, set it again for every block). The stereo kernel passes it to both
// channels. With the bit set and end-start >= 2, a cursor that crosses end going
// forward (or start going in reverse) wraps by the region length end-start, keeps
// the fraction and renders the rest of the block; done stays 0. One wrap only:
// if the wrapped index is still outside [start,end) (rate magnitude above the
// region length) the voice ends as without the loop. A region shorter than 2
// frames does not loop. A forward cursor at end-1 interpolates toward the sample
// at start; in reverse the neighbor stays clamped to end-1. Without the bit the
// output and the state are as before (bit-identical).
// Bounds: 0 <= start < end <= 2^30-1 (empty allowed); initial index in
// [-32768, 2^30+32767], fraction 0..65535, any signed32 rate. The split update
// stays within signed32 even at either bound with extreme rates.
// Empty, finished or outside state reads no source. Caller handles unready resources.
// Instruction ceiling including DK_CALL and three argument loads: 32*80+102 = 2662
// (measured worst case 2430 with the loop option (a 2-frame region wrapping on every output), 1854 without).
// Counts are instructions, not hardware cycles or a performance claim.
.SECTION/PM seg_pmco;
.GLOBAL _dk_sample_pcm16_linear;
.GLOBAL dk_sample_pcm16_linear.;
_dk_sample_pcm16_linear:
dk_sample_pcm16_linear.:
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
    R2 = 1;
    R14 = R14 AND R2;
    R2 = -16;
    R10 = ASHIFT R0 BY R2;
    R11 = R0 AND R13;
    COMP(R5, R6);
    IF GE JUMP _dk_sample_pcm16_finish;
    COMP(R7, R5);
    IF LT JUMP _dk_sample_pcm16_finish;
    COMP(R7, R6);
    IF GE JUMP _dk_sample_pcm16_finish;
_dk_sample_pcm16_loop:
    R0 = PASS R14;
    IF NE JUMP _dk_sample_pcm16_zero;
    R0 = LSHIFT R7 BY -1;
    R0 = R3 + R0;
    I4 = R0;
    R2 = 1;
    R1 = R7 AND R2;
    R1 = LSHIFT R1 BY 4;
    R2 = 16;
    R1 = R2 - R1;
    R0 = DM(0, I4);
    R0 = LSHIFT R0 BY R1;
    R2 = -16;
    R0 = ASHIFT R0 BY R2;
    R2 = -15;
    F0 = FLOAT R0 BY R2;
    R4 = R7 + 1;
    COMP(R4, R6);
    IF LT JUMP _dk_sample_pcm16_neighbor;
    R4 = R7;
    R1 = DM(5, I1);
    R2 = 2;
    R1 = R1 AND R2;
    IF EQ JUMP _dk_sample_pcm16_neighbor;
    R1 = PASS R10;
    IF LT JUMP _dk_sample_pcm16_neighbor;
    R4 = PASS R5;
_dk_sample_pcm16_neighbor:
    R1 = LSHIFT R4 BY -1;
    R1 = R3 + R1;
    I4 = R1;
    R2 = 1;
    R1 = R4 AND R2;
    R1 = LSHIFT R1 BY 4;
    R2 = 16;
    R1 = R2 - R1;
    R4 = DM(0, I4);
    R1 = LSHIFT R4 BY R1;
    R2 = -16;
    R1 = ASHIFT R1 BY R2;
    F4 = FLOAT R9 BY R2;
    R2 = -15;
    F1 = FLOAT R1 BY R2;
    F2 = F1 - F0;
    F1 = F4 * F2;
    F8 = F0 + F1;
    DK_SAMPLE_CURSOR_STEP(R7, R9, R10, R11, R13, R0, R1);
    COMP(R7, R5);
    IF LT JUMP _dk_sample_pcm16_crossed;
    COMP(R7, R6);
    IF GE JUMP _dk_sample_pcm16_crossed;
    JUMP _dk_sample_pcm16_store;
_dk_sample_pcm16_crossed:
    R0 = DM(5, I1);
    R1 = 2;
    R0 = R0 AND R1;
    IF EQ JUMP _dk_sample_pcm16_end;
    R1 = R6 - R5;
    R0 = 2;
    COMP(R1, R0);
    IF LT JUMP _dk_sample_pcm16_end;
    COMP(R7, R5);
    IF LT JUMP _dk_sample_pcm16_wrap_up;
    R7 = R7 - R1;
    JUMP _dk_sample_pcm16_wrapped;
_dk_sample_pcm16_wrap_up:
    R7 = R7 + R1;
_dk_sample_pcm16_wrapped:
    COMP(R7, R5);
    IF LT JUMP _dk_sample_pcm16_end;
    COMP(R7, R6);
    IF LT JUMP _dk_sample_pcm16_store;
_dk_sample_pcm16_end:
    R14 = 1;
    JUMP _dk_sample_pcm16_store;
_dk_sample_pcm16_finish:
    R14 = 1;
_dk_sample_pcm16_zero:
    R8 = 0;
_dk_sample_pcm16_store:
    DM(I0, M6) = R8;
    R0 = R15 - 1;
    R15 = PASS R0;
    IF GT JUMP _dk_sample_pcm16_loop;
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
