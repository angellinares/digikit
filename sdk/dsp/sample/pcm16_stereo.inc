// Planar packed PCM16 stereo numerical reader; no resource ABI or pool policy.
// Include dsp/contract/call.inc first (dsp/sample/float.inc and pcm16.inc may be included too; this file uses neither).
// _dk_sample_pcm16_stereo_linear / dk_sample_pcm16_stereo_linear.:
// R4 = two normal-word source bases [left, right], R8 = two normal-word
// output bases [left, right], R12 = the existing six-word sample cursor.
// Each output has 32 float32 words. Each source has the mono packed PCM16
// layout and bounds of sample/pcm16.inc. Both channels use the same cursor,
// start/end, fraction and signed rate, including reverse and zero rate.
// Numerical contract: float32(int16/32768), interpolation a + frac * (b - a) in the float reader's
// order, the same outputs and cursor as _dk_sample_pcm16_linear run once per channel.
// Source bases may be equal for mono-to-stereo: that case reads one plane and writes both outputs.
// Output buffers must be disjoint. Sources, outputs, cursor, descriptors and stack must otherwise not
// alias; sources/descriptors immutable. All mono bounds, completion and rounding rules apply.
// The loop option (bit 1 of cursor word 5, see sample/pcm16.inc) applies to both
// channels; the caller's word 5 is written back without the bit.
// Structure: one pass for both channels. The index, word addresses, half select and fraction are
// computed once per frame and shared by both planes. The block is cut at the frames where the
// cursor leaves [start,end): a bisection over the constant rate finds the first crossing frame, a hardware
// loop (LCNTR) renders the frames up to and including it without any range check, then the end or
// wrap rule runs (a wrap moves the index by the region length once per crossing; an index still outside after the
// wrap ends the voice) and the next segment starts. The forward neighbor at end-1 is clamped
// without a branch (a conditional move of the precomputed neighbor index). The cursor fraction runs in
// the high half of a register: one add sets the carry, the index add takes it (CI).
// Saves R3,R5,R6,R7,R9,R10,R11,R13,R14,R15 and I0-I3,I5; 20 save slots (word slots 2 to 21).
// Frame convention: the actual adapters enter modules with a byte-alias I6 (byte addresses, a word is 4),
// the same convention as call.inc. A normal-word I6 is not supported (finding 111).
// Relative to this routine's entry I6, the deepest write is word slot 21 (I6 - 84 bytes), I7 ends at slot 22.
// Caller DK_CALL writes I6 and I6-1. Clobbers only call.inc's caller-saved set. No nested calls.
// Instruction counts including caller DK_CALL and 3 argument loads (tests/patchkit/test_dsp_sample_stereo_kernels.py): 1625 for a block that
// stays inside the region (1144 mono-to-stereo), 5152 worst measured (a 2-frame region wrapping at every frame; each crossing
// pays one bisection), bound 5428. Each crossing costs about 150 instructions, a frame about 50.
// Counts are instructions, not hardware cycles or a performance claim.
.SECTION/PM seg_pmco;
.GLOBAL _dk_sample_pcm16_stereo_linear;
.GLOBAL dk_sample_pcm16_stereo_linear.;
_dk_sample_pcm16_stereo_linear:
dk_sample_pcm16_stereo_linear.:
    DK_ENTER(20);
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
    R0 = I2;
    DK_SAVE(14, R0);
    R0 = I3;
    DK_SAVE(15, R0);
    R0 = I5;
    DK_SAVE(16, R0);
    DK_SAVE(17, R12);                     // the cursor
    I1 = R4;
    R3 = DM(0, I1);                       // left plane
    R5 = DM(1, I1);                       // right plane
    I1 = R8;
    R0 = DM(0, I1);
    R1 = DM(1, I1);
    I0 = R0;                              // left output
    I2 = R1;                              // right output
    I1 = R12;
    R0 = DM(0, I1);
    DK_SAVE(18, R0);                      // start
    R6 = DM(1, I1);                       // end
    R7 = DM(2, I1);                       // index
    R9 = DM(3, I1);                       // fraction
    R0 = DM(4, I1);                       // rate
    R14 = DM(5, I1);                      // done, loop bit
    DK_SAVE(20, R14);
    R10 = ASHIFT R0 BY -16;               // rate high half (signed)
    R11 = LSHIFT R0 BY 16;                // rate low half in the high half of the register
    R9 = LSHIFT R9 BY 16;                 // fraction in the high half of the register
    R2 = DM(-18, I6);
    COMP(R2, R6);
    IF GE JUMP _dk_sample_pcm16s_finish;
    COMP(R7, R2);
    IF LT JUMP _dk_sample_pcm16s_finish;
    COMP(R7, R6);
    IF GE JUMP _dk_sample_pcm16s_finish;
    R0 = 1;
    R0 = R14 AND R0;
    IF NE JUMP _dk_sample_pcm16s_finish;
    // Neighbor index for a frame at end-1: end-1 (clamp), or start for a forward loop.
    R14 = R6 - 1;
    R0 = DM(-20, I6);
    R1 = 2;
    R0 = R0 AND R1;
    IF EQ JUMP _dk_sample_pcm16s_nb_done;
    R10 = PASS R10;
    IF LT JUMP _dk_sample_pcm16s_nb_done;
    R14 = PASS R2;
_dk_sample_pcm16s_nb_done:
    DK_SAVE(21, R14);
    R15 = 32;
    DK_SAVE(19, R15);                     // frames left in the block
_dk_sample_pcm16s_seg:
    R15 = DM(-19, I6);
    R1 = LSHIFT R9 BY -16;                // fraction
    R2 = LSHIFT R11 BY -16;               // rate low half
    R10 = PASS R10;
    IF LT JUMP _dk_sample_pcm16s_seg_rev;
    // Forward: the block stays inside when the index after all frames left is below end.
    R0 = R15 * R10 (SSI);
    R13 = R15 * R2 (SSI);
    R13 = R13 + R1;
    R13 = LSHIFT R13 BY -16;
    R0 = R0 + R13;
    R0 = R0 + R7;
    COMP(R0, R6);
    IF LT JUMP _dk_sample_pcm16s_seg_all;
    // Bisection: the largest k in 0..31 whose index after k frames is below end; frames = k + 1.
    R4 = 0;
    R8 = 16;
_dk_sample_pcm16s_fwd_bisect:
    R12 = R4 + R8;
    R0 = R12 * R10 (SSI);
    R13 = R12 * R2 (SSI);
    R13 = R13 + R1;
    R13 = LSHIFT R13 BY -16;
    R0 = R0 + R13;
    R0 = R0 + R7;
    COMP(R0, R6);
    IF LT R4 = PASS R12;
    R8 = LSHIFT R8 BY -1;
    R8 = PASS R8;
    IF NE JUMP _dk_sample_pcm16s_fwd_bisect;
    R4 = R4 + 1;
    JUMP _dk_sample_pcm16s_run;
_dk_sample_pcm16s_seg_rev:
    // Reverse: the block stays inside when the index after all frames left is at least start.
    R14 = DM(-18, I6);
    R0 = R15 * R10 (SSI);
    R13 = R15 * R2 (SSI);
    R13 = R13 + R1;
    R13 = LSHIFT R13 BY -16;
    R0 = R0 + R13;
    R0 = R0 + R7;
    COMP(R0, R14);
    IF GE JUMP _dk_sample_pcm16s_seg_all;
    R4 = 0;
    R8 = 16;
_dk_sample_pcm16s_rev_bisect:
    R12 = R4 + R8;
    R0 = R12 * R10 (SSI);
    R13 = R12 * R2 (SSI);
    R13 = R13 + R1;
    R13 = LSHIFT R13 BY -16;
    R0 = R0 + R13;
    R0 = R0 + R7;
    COMP(R0, R14);
    IF GE R4 = PASS R12;
    R8 = LSHIFT R8 BY -1;
    R8 = PASS R8;
    IF NE JUMP _dk_sample_pcm16s_rev_bisect;
    R4 = R4 + 1;
    JUMP _dk_sample_pcm16s_run;
_dk_sample_pcm16s_seg_all:
    R4 = PASS R15;
_dk_sample_pcm16s_run:
    // R4 = frames to render (1 to 32), R15 = frames left. Render them without a range check.
    R15 = R15 - R4;
    DK_SAVE(19, R15);
    R13 = -15;                            // float scale of an int16 sample
    R15 = -16;                            // float scale of the fraction
    R14 = DM(-21, I6);                    // neighbor index at end-1
    COMP(R3, R5);
    IF EQ JUMP _dk_sample_pcm16s_run_mono;
    LCNTR = R4, DO _dk_sample_pcm16s_stereo_end UNTIL LCE;
        R0 = LSHIFT R7 BY -1;             // word of frame i
        R8 = R3 + R0;
        R12 = R5 + R0;
        I1 = R8;
        I3 = R12;
        R2 = R7 + 1;
        COMP(R2, R6);
        IF GE R2 = PASS R14;              // neighbor index
        R0 = LSHIFT R2 BY -1;             // word of the neighbor
        R8 = R3 + R0;
        R12 = R5 + R0;
        I4 = R8;
        I5 = R12;
        R1 = LSHIFT R7 BY 31;
        R4 = LSHIFT R2 BY 31;
        R1 = BTGL R1 BY 31;
        R4 = BTGL R4 BY 31;
        R1 = LSHIFT R1 BY -27;            // shift that moves the half of frame i to the top: 16 even, 0 odd
        R4 = LSHIFT R4 BY -27;            // the same for the neighbor
        R0 = DM(0, I1);                   // left, frame i
        R8 = DM(0, I3);                   // right, frame i
        R2 = DM(0, I4);                   // left, neighbor
        R12 = DM(0, I5);                  // right, neighbor
        R0 = LSHIFT R0 BY R1;
        R8 = LSHIFT R8 BY R1;
        R2 = LSHIFT R2 BY R4;
        R12 = LSHIFT R12 BY R4;
        R0 = ASHIFT R0 BY -16;
        R8 = ASHIFT R8 BY -16;
        R2 = ASHIFT R2 BY -16;
        R12 = ASHIFT R12 BY -16;
        F0 = FLOAT R0 BY R13;
        F8 = FLOAT R8 BY R13;
        F2 = FLOAT R2 BY R13;
        F12 = FLOAT R12 BY R13;
        R1 = LSHIFT R9 BY -16;
        F1 = FLOAT R1 BY R15;             // fraction / 65536
        F2 = F2 - F0;
        F12 = F12 - F8;
        F2 = F1 * F2;
        F12 = F1 * F12;
        F0 = F0 + F2;
        F8 = F8 + F12;
        DM(I0, M6) = F0;
        DM(I2, M6) = F8;
        R9 = R9 + R11;                    // sets the carry
_dk_sample_pcm16s_stereo_end: R7 = R7 + R10 + CI;
    JUMP _dk_sample_pcm16s_after;
_dk_sample_pcm16s_run_mono:
    LCNTR = R4, DO _dk_sample_pcm16s_mono_end UNTIL LCE;
        R0 = LSHIFT R7 BY -1;
        R8 = R3 + R0;
        I1 = R8;
        R2 = R7 + 1;
        COMP(R2, R6);
        IF GE R2 = PASS R14;
        R0 = LSHIFT R2 BY -1;
        R8 = R3 + R0;
        I4 = R8;
        R1 = LSHIFT R7 BY 31;
        R4 = LSHIFT R2 BY 31;
        R1 = BTGL R1 BY 31;
        R4 = BTGL R4 BY 31;
        R1 = LSHIFT R1 BY -27;
        R4 = LSHIFT R4 BY -27;
        R0 = DM(0, I1);
        R2 = DM(0, I4);
        R0 = LSHIFT R0 BY R1;
        R2 = LSHIFT R2 BY R4;
        R0 = ASHIFT R0 BY -16;
        R2 = ASHIFT R2 BY -16;
        F0 = FLOAT R0 BY R13;
        F2 = FLOAT R2 BY R13;
        R1 = LSHIFT R9 BY -16;
        F1 = FLOAT R1 BY R15;
        F2 = F2 - F0;
        F2 = F1 * F2;
        F0 = F0 + F2;
        DM(I0, M6) = F0;
        DM(I2, M6) = F0;
        R9 = R9 + R11;
_dk_sample_pcm16s_mono_end: R7 = R7 + R10 + CI;
_dk_sample_pcm16s_after:
    R0 = DM(-18, I6);
    COMP(R7, R0);
    IF LT JUMP _dk_sample_pcm16s_crossed;
    COMP(R7, R6);
    IF GE JUMP _dk_sample_pcm16s_crossed;
    R14 = 0;
    JUMP _dk_sample_pcm16s_exit;
_dk_sample_pcm16s_crossed:
    // The index left [start,end) after the last frame rendered. Loop option: wrap once by the region length.
    R0 = DM(-20, I6);
    R1 = 2;
    R0 = R0 AND R1;
    IF EQ JUMP _dk_sample_pcm16s_end_voice;
    R0 = DM(-18, I6);
    R1 = R6 - R0;                         // region length
    R2 = 2;
    COMP(R1, R2);
    IF LT JUMP _dk_sample_pcm16s_end_voice;
    COMP(R7, R0);
    IF LT JUMP _dk_sample_pcm16s_wrap_up;
    R7 = R7 - R1;
    JUMP _dk_sample_pcm16s_wrapped;
_dk_sample_pcm16s_wrap_up:
    R7 = R7 + R1;
_dk_sample_pcm16s_wrapped:
    COMP(R7, R0);
    IF LT JUMP _dk_sample_pcm16s_end_voice;
    COMP(R7, R6);
    IF GE JUMP _dk_sample_pcm16s_end_voice;
    R15 = DM(-19, I6);
    R15 = PASS R15;
    IF GT JUMP _dk_sample_pcm16s_seg;
    R14 = 0;
    JUMP _dk_sample_pcm16s_exit;
_dk_sample_pcm16s_finish:
    R15 = 32;
    DK_SAVE(19, R15);
_dk_sample_pcm16s_end_voice:
    R14 = 1;
    R15 = DM(-19, I6);                    // outputs left to silence
    R0 = 0;
    R15 = PASS R15;
    IF LE JUMP _dk_sample_pcm16s_exit;
_dk_sample_pcm16s_zero:
    DM(I0, M6) = R0;
    DM(I2, M6) = R0;
    R15 = R15 - 1;
    IF GT JUMP _dk_sample_pcm16s_zero;
_dk_sample_pcm16s_exit:
    R9 = LSHIFT R9 BY -16;
    R12 = DM(-17, I6);
    I1 = R12;
    DK_RETADDR;
    DM(2, I1) = R7;
    DM(3, I1) = R9;
    DM(5, I1) = R14;
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
    DK_RESTORE(I2, 14);
    DK_RESTORE(I3, 15);
    DK_RESTORE(I5, 16);
    DK_LEAVE(I1, 13);
