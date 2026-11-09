// Private synthetic replay of b80345, not the stock routing or a stereo ABI.
// Call arguments are control, byte control, count; audio pointers are bytes.
#include "dsp/call.inc"
.SECTION/PM seg_pmco;
.GLOBAL _prepare_gain;
_prepare_gain:
    I4 = 0x08040000; I0 = 0x08040100; R1 = 11;
_copy_gain:
    R0 = DM(I4, M6); DM(I0, M6) = R0;
    R1 = R1 - 1; IF NE JUMP _copy_gain;
    RTS;

#define GAIN_STAGE(state, audio, label) \
    R4 = state; R8 = audio; R12 = audio; \
    R2 = 32; DM(I7, M7) = R2; \
    DM(I7, M7) = R1; DM(I7, M7) = R0; \
    DK_CALL(_gain, label); MODIFY(I7, 3) (NW); RTS

.GLOBAL _left_gain;
_left_gain:
    GAIN_STAGE(0x20100000, 0x20104000, _lg);
.GLOBAL _right_gain;
_right_gain:
    GAIN_STAGE(0x20100400, 0x20104400, _rg);
