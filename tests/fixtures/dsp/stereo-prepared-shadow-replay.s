// Assembled after stereo-filter-shadow-replay.s; reuse COPY_WORD and STAGE.
// Private synthetic replay, not stock routing or a production stereo ABI.
.GLOBAL _prepare_held;
_prepare_held:
    I4 = 0x08040000; I0 = 0x08040100;
    COPY_WORD(0); COPY_WORD(3); COPY_WORD(4);
    RTS;
.GLOBAL _prepare_prepared;
_prepare_prepared:
    I4 = 0x08040000; I0 = 0x08040100;
    COPY_WORD(0); COPY_WORD(1); COPY_WORD(2); COPY_WORD(3); COPY_WORD(4);
    COPY_WORD(14);
    I4 = 0x08040011; I0 = 0x08040111; R1 = 6;
_copy_prepared:
    COPY_WORD(0); COPY_WORD(1); COPY_WORD(2);
    MODIFY(I4, 7) (NW); MODIFY(I0, 7) (NW);
    R1 = R1 - 1; IF NE JUMP _copy_prepared;
    RTS;

.GLOBAL _left_held;
_left_held:
    STAGE(_held, 0x20100000, 0x20104000, _lh);
.GLOBAL _right_held;
_right_held:
    STAGE(_held, 0x20100400, 0x20104400, _rh);
.GLOBAL _left_prepared;
_left_prepared:
    STAGE(_prepared, 0x20100000, 0x20104000, _lp);
.GLOBAL _right_prepared;
_right_prepared:
    STAGE(_prepared, 0x20100400, 0x20104400, _rp);
