// Block gain ramp for engines; numerical primitive, no sample or voice policy.
// Use it for a constant gain (step 0), an end fade (start -> 0), and as the basis for adjustable fade in/out.
// The sample kernels stay without fades and gains; shaping is done here, after the kernel.
// _dk_gain_ramp_stereo / dk_gain_ramp_stereo. : dsp/contract/call.inc convention, 32 samples per channel (the block size).
// R4 = left buffer, R8 = right buffer (normal-word DM addresses of 32 float32 words each),
// R12 = a pair of float32 words in the caller's memory, same address form as the sample kernels' R12: [start, step].
// For k = 0 .. 31: left[k] *= g and right[k] *= g, with g = start at k = 0 and g = g - step after each sample (float32).
// The buffers are scaled in place. Finite values are required. There is no clamping: the caller chooses step.
// After the block, pair[0] holds the gain reached, start - 32 * step (32 float32 subtractions), so the next
// block continues the ramp with the same pair. pair[1] (step) is not written.
// Left, right and the pair must not overlap. Needs M6 = 1 (the module contract holds it); L0-L7 = 0.
// Saves R3,R6,R7,I0,I1 (5 slots); call.inc clobbers otherwise (also I4 and the loop counter). MODE1 is unchanged.
// Instruction ceiling with DK_CALL and 3 argument loads: 7 * 32 + 30 = 254; measured counts in the module tests.
// Sequential implementation; instructions are not hardware cycles.
// Needs dsp/contract/call.inc included first. Does not depend on DK_BLOCK or the sample includes.
.SECTION/PM seg_pmco;
.GLOBAL _dk_gain_ramp_stereo;
.GLOBAL dk_gain_ramp_stereo.;
_dk_gain_ramp_stereo:
dk_gain_ramp_stereo.:
    DK_ENTER(5);
    DK_SAVE(2, R3);
    DK_SAVE(3, R6);
    DK_SAVE(4, R7);
    R0 = I1;
    DK_SAVE(6, R0);
    I1 = R12;
    R0 = I0;
    DK_SAVE(5, R0);
    I0 = R4;
    I4 = R8;
    R7 = DM(0, I1);                       // g
    R6 = DM(1, I1);                       // step
    LCNTR = 32, DO _dk_gain_ramp_end UNTIL LCE;
        R2 = DM(0, I0);
        F2 = F2 * F7;
        DM(I0, M6) = R2;
        R3 = DM(0, I4);
        F3 = F3 * F7;
        DM(I4, M6) = R3;
_dk_gain_ramp_end: F7 = F7 - F6;
    DM(0, I1) = R7;
    DK_RETADDR;
    DK_RESTORE(R3, 2);
    DK_RESTORE(R6, 3);
    DK_RESTORE(R7, 4);
    DK_RESTORE(I0, 5);
    DK_LEAVE(I1, 6);
