// Stock callees use byte-space pointers and normal-word accesses.
// Synthetic leaf replay only. Case-5 scratch ownership/blend is not exercised.
#include "dsp/call.inc"
.SECTION/PM seg_pmco;
#define COPY_WORD(n) R0 = DM(n, I4); DM(n, I0) = R0

.GLOBAL _prepare_snapshots;
_prepare_snapshots:
    I4 = 0x08040000; I0 = 0x08040100;
    // Keep 3/4/48/49/50 private, including the audio-dependent snapshots.
    COPY_WORD(5); COPY_WORD(6); COPY_WORD(7);
    COPY_WORD(12); COPY_WORD(14); COPY_WORD(26); COPY_WORD(28);
    COPY_WORD(33); COPY_WORD(35); COPY_WORD(40); COPY_WORD(42);
    RTS;

.GLOBAL _prepare_nine;
_prepare_nine:
    I4 = 0x0804000c; I0 = 0x0804010c; R1 = 9;
    JUMP _copy_pairs;
.GLOBAL _prepare_six;
_prepare_six:
    I4 = 0x0804000b; I0 = 0x0804010b; R1 = 6;
_copy_pairs:
    COPY_WORD(0); COPY_WORD(2);
    MODIFY(I4, 7) (NW); MODIFY(I0, 7) (NW);
    R1 = R1 - 1; IF NE JUMP _copy_pairs;
    RTS;

#define STAGE(fn, state, audio, label) \
    R4 = state; R8 = audio; R12 = audio; \
    R0 = 32; DM(I7, M7) = R0; \
    DK_CALL(fn, label); MODIFY(I7, 1) (NW); RTS

.GLOBAL _left_snapshots;
_left_snapshots:
    STAGE(_snapshots, 0x20100000, 0x20104000, _ls);
.GLOBAL _right_snapshots;
_right_snapshots:
    STAGE(_snapshots, 0x20100400, 0x20104400, _rs);
.GLOBAL _left_nine;
_left_nine:
    STAGE(_nine, 0x20100000, 0x20104000, _ln);
.GLOBAL _right_nine;
_right_nine:
    STAGE(_nine, 0x20100400, 0x20104400, _rn);
.GLOBAL _left_six;
_left_six:
    STAGE(_six, 0x20100000, 0x20104000, _lx);
.GLOBAL _right_six;
_right_six:
    STAGE(_six, 0x20100400, 0x20104400, _rx);
