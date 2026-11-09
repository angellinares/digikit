// Synthetic audited subset, NOT the stock default route or a public SDK ABI.
// Normal-word L2 addresses: controls 0x08040000/100, first-order
// 0x08040200/300, U/Y 0x08040400/500, audio 0x08041000/100.
#include "dsp/call.inc"
.SECTION/PM seg_pmco;
.GLOBAL _prepare;
_prepare:
    I4 = 0x08040000; I0 = 0x08040100;
    R1 = 14;
_copy_controls:
    R0 = DM(I4, M6); DM(I0, M6) = R0;
    R1 = R1 - 1; IF NE JUMP _copy_controls;
    I4 = 0x08040215; I0 = 0x08040315;
    R1 = 3;
_copy_first:
    R0 = DM(I4, M6); DM(I0, M6) = R0;
    R1 = R1 - 1; IF NE JUMP _copy_first;
    I4 = 0x08040402; I0 = 0x08040502;
    R1 = 4;
_copy_uy:
    R0 = DM(I4, M6); DM(I0, M6) = R0;
    R1 = R1 - 1; IF NE JUMP _copy_uy;
    RTS;

#define STAGE(fn, state, audio, label) \
    R4 = state; R8 = audio; R12 = audio; \
    R0 = 32; DM(I7, M7) = R0; \
    DK_CALL(fn, label); MODIFY(I7, 1) (NW)

.GLOBAL _left;
_left:
    STAGE(_control, 0x08040000, 0x08041000, _lc1);
    STAGE(_first, 0x08040200, 0x08041000, _lf);
    STAGE(_uy, 0x08040400, 0x08041000, _lu);
    // Repeated occurrence intentionally reuses mutations from _lc1.
    STAGE(_control, 0x08040000, 0x08041000, _lc2);
    RTS;
.GLOBAL _right;
_right:
    STAGE(_control, 0x08040100, 0x08041100, _rc1);
    STAGE(_first, 0x08040300, 0x08041100, _rf);
    STAGE(_uy, 0x08040500, 0x08041100, _ru);
    STAGE(_control, 0x08040100, 0x08041100, _rc2);
    RTS;

// Default-core recurrence-only control, bypassing RECIPS-using 1cdb56.
.GLOBAL _recurrences;
_recurrences:
    STAGE(_first, 0x08040200, 0x08041000, _df);
    STAGE(_uy, 0x08040400, 0x08041000, _du);
    RTS;

.GLOBAL _right_recurrences;
_right_recurrences:
    STAGE(_first, 0x08040300, 0x08041100, _drf);
    STAGE(_uy, 0x08040500, 0x08041100, _dru);
    RTS;
