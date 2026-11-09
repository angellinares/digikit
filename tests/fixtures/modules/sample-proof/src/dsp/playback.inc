// Actual machine consumer shared with the numerical tests. Seven state words:
// kernel start,end,index,fraction,rate,done; latched source pointer.
.SECTION/PM seg_pmco;
.GLOBAL _sp_render;
.GLOBAL _sp_note_on;
.EXTERN _sp_view;
_sp_render:
    // Module entry is a JUMP, not a compiler CJUMP frame. I7 points at
    // the exclusive edge of the adapter frame: reserve one word before
    // DK_CALL's post-modify stores. The kernel preserves R3/R5 but uses I12
    // as its return scratch, so bridge both module return registers here.
    R3 = I12;
    R5 = I13;
    MODIFY(I7, -1) (NW);
    I3 = R4;
    I2 = R8;
    R4 = DM(6, I3);
    R8 = DM(DK_IN_W_OUT, I2);
    R12 = I3;
    DK_CALL(_dk_sample_linear, _sp_after_linear);
    MODIFY(I7, 1) (NW);
    I12 = R3;
    I13 = R5;
    R0 = DM(5, I3);
    NOP;
    NOP;
    NOP;
    JUMP (M13, I13);

_sp_note_on:
    I3 = R4;
    I2 = R12;
    R0 = 0;
    DM(0, I3) = R0;
    DM(1, I3) = R0;
    DM(2, I3) = R0;
    DM(3, I3) = R0;
    DM(6, I3) = R0;
    R0 = 1;
    DM(5, I3) = R0;
    R0 = 65536;
    DM(4, I3) = R0;
    I4 = _sp_view;
    R0 = DM(0, I4);
    R1 = 1;
    COMP(R0, R1);
    IF NE JUMP _sp_note_return;
    R1 = DM(1, I4);
    DM(6, I3) = R1;
    R2 = DM(2, I4);
    R0 = DM(0, I2);
    R3 = -8;
    R0 = LSHIFT R0 BY R3;
    R1 = DM(1, I2);
    R1 = LSHIFT R1 BY R3;
    R3 = 127;
    COMP(R1, R3);
    IF EQ R1 = PASS R2;
    R0 = MIN(R0, R2);
    R1 = MIN(R1, R2);
    DM(0, I3) = R0;
    DM(1, I3) = R1;
    COMP(R0, R1);
    IF GE JUMP _sp_note_return;
    DM(2, I3) = R0;
    R2 = 0;
    DM(5, I3) = R2;
    R2 = DM(2, I2);
    R3 = 16384;
    COMP(R2, R3);
    IF LT JUMP _sp_note_return;
    R0 = R1 - 1;
    DM(2, I3) = R0;
    R0 = -65536;
    DM(4, I3) = R0;
_sp_note_return:
    R0 = 0;
    JUMP (M13, I13);
