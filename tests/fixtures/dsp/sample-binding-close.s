// Private complete immutable logical messages; pointers are normal-word DM.
#include "sample-bindings.s"
.SECTION/DM seg_dmda;
.VAR _control_last_id = 0;
.VAR _control_last_track = 0;
.VAR _control_last_epoch = 0;
.VAR _control_last_drained = 0;
.SECTION/PM seg_pmco;
.GLOBAL _bind_control;
_bind_control:
    // Custom leaf entry like _bind_close; reserve private saves below caller SP.
    MODIFY(I7, -9) (NW);
    DM(1, I7) = R3; DM(2, I7) = R5; DM(3, I7) = R6;
    DM(4, I7) = R7; DM(5, I7) = R9;
    R0 = I4; DM(6, I7) = R0;
    R0 = I12; DM(7, I7) = R0;
    R0 = I13; DM(8, I7) = R0;
    R9 = R8;
    I4 = R4;
    NOP; NOP; NOP;
    R0 = DM(0, I4); R5 = DM(1, I4);
    R6 = DM(2, I4); R3 = DM(3, I4);
    R1 = 0x01014243; COMP(R0, R1); IF NE JUMP _control_reject;
    R5 = PASS R5; IF LE JUMP _control_reject;
    R3 = PASS R3; IF LE JUMP _control_reject;
    R0 = 0; COMP(R6, R0); IF LT JUMP _control_reject;
    R0 = 1; COMP(R6, R0); IF GT JUMP _control_reject;
    R0 = DM(_control_last_id);
    COMP(R5, R0); IF LT JUMP _control_reject;
    IF EQ JUMP _control_retry;
    R0 = PASS R0; IF EQ JUMP _control_identity;
    R0 = DM(_control_last_drained); R0 = PASS R0;
    IF EQ JUMP _control_reject;
    JUMP _control_identity;
_control_retry:
    R0 = DM(_control_last_track); COMP(R6, R0); IF NE JUMP _control_reject;
    R0 = DM(_control_last_epoch); COMP(R3, R0); IF NE JUMP _control_reject;
_control_identity:
    R0 = PASS R6; IF NE JUMP _control_b_identity;
    R0 = DM(_bind_a_token); JUMP _control_check_epoch;
_control_b_identity:
    R0 = DM(_bind_b_token);
_control_check_epoch:
    COMP(R3, R0); IF NE JUMP _control_reject;
    // Identity checked before adopting a new request; rejected commands do not
    // replace accepted history. Exact retries always recheck the live bank.
    R0 = DM(_control_last_id); COMP(R5, R0); IF EQ JUMP _control_accept;
    R0 = 0; DM(_control_last_drained) = R0;
_control_accept:
    DM(_control_last_id) = R5;
    DM(_control_last_track) = R6;
    DM(_control_last_epoch) = R3;
    R4 = R6; R8 = R3;
    DK_CALL(_control_close_call, _control_after_close);
    R0 = PASS R0; IF EQ JUMP _control_drained;
    R1 = 2; COMP(R0, R1); IF NE JUMP _control_reject;
    R2 = 1; JUMP _control_reply;
_control_drained:
    R0 = 1; DM(_control_last_drained) = R0;
    R2 = 2; JUMP _control_reply;
_control_reject:
    R2 = 3;
_control_reply:
    R0 = LSHIFT R2 BY 24;
    R1 = 0x00014241; R0 = R0 OR R1;
    I4 = R9;
    NOP; NOP; NOP;
    DM(0, I4) = R0; DM(1, I4) = R5;
    DM(2, I4) = R6; DM(3, I4) = R3;
    R0 = R2;
    R1 = DM(6, I7); I4 = R1;
    R1 = DM(7, I7); I12 = R1;
    R1 = DM(8, I7); I13 = R1;
    R3 = DM(1, I7); R5 = DM(2, I7); R6 = DM(3, I7);
    R7 = DM(4, I7); R9 = DM(5, I7);
    MODIFY(I7, 9) (NW);
    JUMP (M13, I13);
_control_close_call:
    // Adapt the fixture's leaf close to a DK_CALL frame, preserving outer I13.
    I13 = _control_close_return;
    JUMP _bind_close;
_control_close_return:
    DK_RETADDR;
    NOP; NOP; NOP; NOP;
    JUMP (M14, I12) (DB);
    NOP;
    RFRAME;
