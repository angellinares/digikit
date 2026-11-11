// Planar packed PCM16 stereo numerical reader; no resource ABI or pool policy.
// Include dsp/contract/call.inc, dsp/sample/float.inc and dsp/sample/pcm16.inc first.
// _dk_sample_pcm16_stereo_linear / dk_sample_pcm16_stereo_linear.:
// R4 = two normal-word source bases [left, right], R8 = two normal-word
// output bases [left, right], R12 = the existing six-word sample cursor.
// Each output has 32 float32 words. Each source has the mono packed PCM16
// layout and bounds of sample/pcm16.inc. Both channels use the same initial
// cursor, start/end, fraction and signed rate, including reverse and zero rate.
// Caller cursor is advanced once by the left call; right advances a stack copy
// of the initial cursor to the identical result. Source bases may be equal for
// mono-to-stereo. Output buffers must be disjoint. Sources, outputs, cursor,
// descriptors and stack must otherwise not alias; sources/descriptors immutable.
// All mono bounds, completion, float rounding and call.inc requirements apply.
// The loop option (bit 1 of cursor word 5, see sample/pcm16.inc) applies to both
// channels; the caller's word 5 is written back without the bit.
// Saves R3,R5,R6,I1: 4 slots plus 6 cursor words = 10 local words.
// Frame convention: the actual adapters enter modules with a byte-alias I6 (byte addresses, a word is 4), so the
// stack copy of the cursor is at I6 - 44 (word slot 11) and the pointer arithmetic counts bytes. A normal-word
// I6 is not supported (finding 111).
// Relative to this routine's entry I6, deepest write is word slot 25 (I6 - 100 bytes) and deepest I7
// is word slot 26 (includes nested call frame and mono's 12 saves). Caller DK_CALL
// writes I6 and I6-1. Clobbers only call.inc's caller-saved set.
// Conservative instruction ceiling including caller DK_CALL and 3 argument
// loads: 2*(32*80+102)+100 = 5428 (measured 4897 with the loop option, 3745 without). Sequential composition; this is an
// instruction bound, not hardware cycles or a hardware performance claim.
.SECTION/PM seg_pmco;
.GLOBAL _dk_sample_pcm16_stereo_linear;
.GLOBAL dk_sample_pcm16_stereo_linear.;
_dk_sample_pcm16_stereo_linear:
dk_sample_pcm16_stereo_linear.:
    DK_ENTER(10);
    DK_SAVE(2, R3);
    DK_SAVE(3, R5);
    DK_SAVE(4, R6);
    R0 = I1;
    DK_SAVE(5, R0);
    I1 = R4;
    R4 = DM(0, I1);
    R3 = DM(1, I1);
    I1 = R8;
    R8 = DM(0, I1);
    R5 = DM(1, I1);
    I1 = R12;
    R0 = DM(0, I1);
    DK_SAVE(11, R0);
    R0 = DM(1, I1);
    DK_SAVE(10, R0);
    R0 = DM(2, I1);
    DK_SAVE(9, R0);
    R0 = DM(3, I1);
    DK_SAVE(8, R0);
    R0 = DM(4, I1);
    DK_SAVE(7, R0);
    R0 = DM(5, I1);
    DK_SAVE(6, R0);
    R1 = I6;
    R0 = 44;
    R6 = R1 - R0;
    DK_CALL(_dk_sample_pcm16_linear, _dk_sample_stereo_after_left);
    R4 = R3;
    R8 = R5;
    R12 = R6;
    DK_CALL(_dk_sample_pcm16_linear, _dk_sample_stereo_after_right);
    DK_RETADDR;
    DK_RESTORE(R3, 2);
    DK_RESTORE(R5, 3);
    DK_RESTORE(R6, 4);
    DK_LEAVE(I1, 5);
