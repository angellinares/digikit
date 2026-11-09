// Synthetic selected route only. Scalar/later ramps are unity; other
// conditionals/fade and shared cleanup are excluded, not replayed twice.
#include "dsp/call.inc"
.SECTION/PM seg_pmco;
#define COPY_ORDERED(n) R0 = DM(n, I4); DM(n, I0) = R0
.GLOBAL _prepare_ordered;
_prepare_ordered:
    I4 = 0x08040000; I0 = 0x08040100; COPY_ORDERED(0);
    I4 = 0x08040200; I0 = 0x08040300;
    COPY_ORDERED(21); COPY_ORDERED(22); COPY_ORDERED(23);
    I4 = 0x0804040b; I0 = 0x0804050b; R1 = 6;
_copy_ordered_case5:
    COPY_ORDERED(0); COPY_ORDERED(2);
    MODIFY(I4, 7) (NW); MODIFY(I0, 7) (NW);
    R1 = R1 - 1; IF NE JUMP _copy_ordered_case5;
    I4 = 0x08040600; I0 = 0x08040700;
    COPY_ORDERED(0); COPY_ORDERED(2); COPY_ORDERED(7); COPY_ORDERED(9);
    I4 = 0x08040800; I0 = 0x08040900; R1 = 11;
_copy_ordered_gain:
    R0 = DM(I4, M6); DM(I0, M6) = R0;
    R1 = R1 - 1; IF NE JUMP _copy_ordered_gain;
    I4 = 0x08040a00; I0 = 0x08040b00;
    COPY_ORDERED(0); COPY_ORDERED(2); COPY_ORDERED(112); COPY_ORDERED(114);
    RTS;
#define ORDERED_STAGE(fn, state, input, output, label) \
    R4 = state; R8 = input; R12 = output; \
    R0 = 32; DM(I7, M7) = R0; \
    DK_CALL(fn, label); MODIFY(I7, 1) (NW)
.GLOBAL _left_ordered;
_left_ordered:
    ORDERED_STAGE(_source, 0x20100000, 0x20104000, 0x20104000, _los);
    ORDERED_STAGE(_first, 0x20100800, 0x20104000, 0x20104000, _lof);
    ORDERED_STAGE(_case5, 0x20101000, 0x20104000, 0x20104800, _loc);
    RTS;
.GLOBAL _right_ordered;
_right_ordered:
    ORDERED_STAGE(_source, 0x20100400, 0x20104400, 0x20104400, _ros);
    ORDERED_STAGE(_first, 0x20100c00, 0x20104400, 0x20104400, _rof);
    ORDERED_STAGE(_case5, 0x20101400, 0x20104400, 0x20104c00, _roc);
    RTS;
#define ORDERED_GAIN(state, audio, label) \
    R4 = state; R8 = audio; R12 = audio; \
    R2 = 32; DM(I7, M7) = R2; \
    DM(I7, M7) = R1; DM(I7, M7) = R0; \
    DK_CALL(_gain, label); MODIFY(I7, 3) (NW); RTS
.GLOBAL _left_ordered_gain;
_left_ordered_gain:
    ORDERED_GAIN(0x20102000, 0x20104000, _log);
.GLOBAL _right_ordered_gain;
_right_ordered_gain:
    ORDERED_GAIN(0x20102400, 0x20104400, _rog);
