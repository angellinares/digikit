#include "dsp/call.inc"
// Private DN2 1.12 identity/polarity observer. Left state/output stay authoritative.
// Wrapper scratch Dregs are caller-clobbered R0/R1/R2/R4/R8/R12 only.
// No previously untouched callee-saved floating register is narrowed.
// STKYX R/W FP sticky fields (PRM29-88..90) are restored in scalar mode
// to the authoritative left result. This does not mask/recover FP interrupts.
// Stock entry uses linked delayed C frames; the trailing render count is
// DM(I6+1). Incoming I12 is dead even on the stock zero-count bypass.
// SIMD rejection: PRM Rev1.5 Table4-22 (printed4-54), Conditional Branches
// and Table4-32 (PDF166): the scalar-entry branch requires NOT-TF in
// PEx, hence cannot pass when PEYEN is set, whatever PEy TF contains.
// Complementary-target UREG restores broadcast in SIMD, so reject with
// PEYEN temporarily clear only while restoring ASTATX; no IRQ/bank bit changes.
// PRM SIMD Mode (PDF102) specifies one-cycle PEYEN latency; two NOPs
// follow each change. Type18a (printed17-2..3) SET/CLR leave BTF alone.
.SECTION/PM seg_pmco;
.EXTERN _sp_stock_render;
.EXTERN _sp_stock_reset;
.EXTERN _sp_engine_base;
.GLOBAL _sp_render;
_sp_render:
    I12 = ASTATX;
    BIT TST MODE1 0x200000;
    NOP; NOP;
    IF NOT TF JUMP _sp_render_scalar;
    BIT CLR MODE1 0x200000;
    NOP; NOP;
    ASTATX = I12;
    BIT SET MODE1 0x200000;
    NOP; NOP;
    JUMP _sp_stock_render;
_sp_render_scalar:
    ASTATX = I12;
    DK_ENTER(32);
    DM(-2, I6) = R0;
    DM(-3, I6) = R1;
    DM(-4, I6) = R2;
    DM(-6, I6) = R4;
    DM(-7, I6) = R8;
    DM(-8, I6) = R12;
    R0 = I0; DM(-9, I6) = R0;
    R0 = I1; DM(-10, I6) = R0;
    R0 = I4; DM(-11, I6) = R0;
    R0 = L0; DM(-13, I6) = R0;
    R0 = L1; DM(-14, I6) = R0;
    R0 = I12; DM(-12, I6) = R0;
    R0 = STKYX; DM(-28, I6) = R0;
    R0 = 0; L0 = R0; L1 = R0;
    R0 = DM(_sp_render_calls); R0 = R0 + 1; DM(_sp_render_calls) = R0;
    // Raw entry pointers, before any nested stock frame. No headroom claim.
    R0 = I6; R1 = DM(_sp_low_entry_frame); R2 = 0; COMP(R1, R2);
    IF EQ JUMP _sp_render_new_frame;
    COMP(R0, R1); IF GE JUMP _sp_render_frame_done;
_sp_render_new_frame:
    DM(_sp_low_entry_frame) = R0;
_sp_render_frame_done:
    I0 = I7; MODIFY(I0, 32) (NW); R0 = I0;
    R1 = DM(_sp_low_entry_sp); R2 = 0; COMP(R1, R2);
    IF EQ JUMP _sp_render_new_sp;
    COMP(R0, R1); IF GE JUMP _sp_render_sp_done;
_sp_render_new_sp:
    DM(_sp_low_entry_sp) = R0;
_sp_render_sp_done:

    R0 = DM(1, I6); R1 = 32; COMP(R0, R1);
    IF NE JUMP _sp_render_fallback;
    // Private right pointers require linear stock state/input/output DAGs.
    R0 = L3; R1 = L4; R0 = R0 OR R1; R1 = L5; R0 = R0 OR R1;
    R1 = 0; COMP(R0, R1); IF NE JUMP _sp_render_fallback;
    R0 = DM(-6, I6); R1 = _sp_engine_base; R2 = 0xe088; R1 = R1 + R2; R0 = R0 - R1;
    IF LT JUMP _sp_render_fallback;
    R1 = 0; I1 = _sp_states; I0 = _sp_audio;
_sp_render_slot:
    R2 = 0; COMP(R0, R2);
    IF EQ JUMP _sp_render_slot_ok;
    R2 = 112; R0 = R0 - R2;
    IF LT JUMP _sp_render_fallback;
    MODIFY(I1, 112); MODIFY(I0, 128);
    R1 = R1 + 1; R2 = 16; COMP(R1, R2);
    IF LT JUMP _sp_render_slot;
    JUMP _sp_render_fallback;
_sp_render_slot_ok:
    // Mode is private, set while paused before any accepted call; immutable thereafter.
    R0 = DM(_sp_mode); R2 = 0; COMP(R0, R2);
    IF LT JUMP _sp_render_mode_error;
    R2 = 1; COMP(R0, R2); IF GT JUMP _sp_render_mode_error;
    R2 = DM(_sp_active_mode); R4 = -1; COMP(R2, R4);
    IF NE JUMP _sp_render_mode_check;
    DM(_sp_active_mode) = R0;
    R4 = MODE1; DM(_sp_numeric_mode) = R4;
_sp_render_mode_check:
    R4 = -1; COMP(R2, R4); IF EQ JUMP _sp_render_mode_ok;
    COMP(R0, R2); IF NE JUMP _sp_render_mode_error;
_sp_render_mode_ok:
    R0 = LSHIFT R0 BY 31; DM(-29, I6) = R0;
    R0 = DM(_sp_mode_errors); R2 = 0; COMP(R0, R2); IF NE JUMP _sp_render_fallback;
    R0 = DM(_sp_nonfinite); R2 = 0; COMP(R0, R2);
    IF NE JUMP _sp_render_fallback;
    DM(-15, I6) = R1;
    R0 = I1; DM(-16, I6) = R0;
    R0 = I0; DM(-17, I6) = R0;
    R0 = LSHIFT R1 BY 2; R2 = _sp_initialized; R0 = R0 + R2;
    I0 = R0; DM(-18, I6) = R0;
    R0 = DM(0, I0); R1 = 0; COMP(R0, R1);
    IF NE JUMP _sp_render_coeff;
    I0 = DM(-6, I6); I1 = DM(-16, I6); R1 = 28;
_sp_clone:
    R0 = DM(0, I0); DM(0, I1) = R0;
    MODIFY(I0, 4); MODIFY(I1, 4);
    R1 = R1 - 1; IF NE JUMP _sp_clone;
    // Only live audio histories24/25 participate in the odd recurrence.
    I1 = DM(-16, I6); R2 = DM(-29, I6);
    R0 = DM(24, I1); R0 = R0 XOR R2; DM(24, I1) = R0;
    R0 = DM(25, I1); R0 = R0 XOR R2; DM(25, I1) = R0;
    I0 = DM(-18, I6); R0 = 1; DM(0, I0) = R0;
_sp_render_coeff:
    I0 = DM(-6, I6); I1 = DM(-16, I6);
    R0 = DM(21, I0); DM(21, I1) = R0;
    R0 = DM(22, I0); DM(22, I1) = R0;
    R0 = DM(23, I0); DM(23, I1) = R0;
    R0 = DM(-29, I6); R2 = 0; COMP(R0, R2); IF EQ JUMP _sp_finite_state_done;
    R0 = DM(21, I0); R2 = 0x7f800000; R0 = R0 AND R2;
    COMP(R0, R2); IF EQ JUMP _sp_render_nonfinite;
    R0 = DM(22, I0); R2 = 0x7f800000; R0 = R0 AND R2;
    COMP(R0, R2); IF EQ JUMP _sp_render_nonfinite;
    R0 = DM(23, I0); R2 = 0x7f800000; R0 = R0 AND R2;
    COMP(R0, R2); IF EQ JUMP _sp_render_nonfinite;
    R0 = DM(24, I0); R2 = 0x7f800000; R0 = R0 AND R2;
    COMP(R0, R2); IF EQ JUMP _sp_render_nonfinite;
    R0 = DM(25, I0); R2 = 0x7f800000; R0 = R0 AND R2;
    COMP(R0, R2); IF EQ JUMP _sp_render_nonfinite;
    R0 = DM(24, I1); R2 = 0x7f800000; R0 = R0 AND R2;
    COMP(R0, R2); IF EQ JUMP _sp_render_nonfinite;
    R0 = DM(25, I1); R2 = 0x7f800000; R0 = R0 AND R2;
    COMP(R0, R2); IF EQ JUMP _sp_render_nonfinite;
_sp_finite_state_done:
    R0 = DM(21, I1); R1 = 0x3f800000; COMP(R0, R1);
    IF NE JUMP _sp_nonidentity;
    R0 = DM(22, I1); R1 = DM(23, I1); R0 = R0 OR R1;
    R1 = 0; COMP(R0, R1); IF EQ JUMP _sp_copy_input;
_sp_nonidentity:
    R0 = DM(_sp_nonidentity_coeff); R1 = 0; COMP(R0, R1);
    IF NE JUMP _sp_nonidentity_count;
    R0 = DM(-15, I6); DM(_sp_coeff_slot) = R0;
    R0 = DM(21, I1); DM(_sp_coeff21) = R0;
    R0 = DM(22, I1); DM(_sp_coeff22) = R0;
    R0 = DM(23, I1); DM(_sp_coeff23) = R0;
_sp_nonidentity_count:
    R0 = DM(_sp_nonidentity_coeff); R0 = R0 + 1; DM(_sp_nonidentity_coeff) = R0;
_sp_copy_input:
    I0 = DM(-7, I6); I1 = DM(-17, I6); R1 = 32;
_sp_input_loop:
    R0 = DM(0, I0); R2 = DM(-29, I6); R4 = 0; COMP(R2, R4);
    IF EQ JUMP _sp_input_finite;
    R2 = 0x7f800000; R4 = R0 AND R2;
    COMP(R4, R2); IF EQ JUMP _sp_render_nonfinite;
_sp_input_finite:
    R2 = DM(-29, I6); R0 = R0 XOR R2; DM(0, I1) = R0;
    R2 = 0x7fffffff; R2 = R0 AND R2;
    R4 = 0; COMP(R2, R4); IF EQ JUMP _sp_input_zero;
    R0 = DM(_sp_nonzero_input); R0 = R0 + 1; DM(_sp_nonzero_input) = R0;
_sp_input_zero:
    MODIFY(I0, 4); MODIFY(I1, 4);
    R1 = R1 - 1; IF NE JUMP _sp_input_loop;
    R0 = DM(1, I6); DM(I7, M7) = R0;
    R0 = DM(-13, I6); L0 = R0;
    R0 = DM(-14, I6); L1 = R0;
    R0 = DM(-9, I6); I0 = R0;
    R0 = DM(-10, I6); I1 = R0;
    R0 = DM(-11, I6); I4 = R0;
    R1 = DM(-3, I6);
    R2 = DM(-4, I6);
    R4 = DM(-6, I6);
    R8 = DM(-7, I6);
    R12 = DM(-8, I6);
    R0 = DM(-28, I6); STKYX = R0;
    R0 = DM(-12, I6); ASTATX = R0;
    R0 = DM(-2, I6);
    DK_CALL(_sp_stock_render, _sp_render_left_return);
    MODIFY(I7, 1) (NW);
    DM(-19, I6) = R0;
    DM(-20, I6) = R1;
    DM(-21, I6) = R2;
    DM(-22, I6) = R4;
    DM(-23, I6) = R8;
    DM(-24, I6) = R12;
    R0 = I4; DM(-25, I6) = R0;
    R0 = ASTATX; DM(-26, I6) = R0;
    R0 = STKYX; DM(-27, I6) = R0;
    R0 = 0; L0 = R0; L1 = R0;
    R0 = DM(_sp_shadow_calls); R0 = R0 + 1; DM(_sp_shadow_calls) = R0;
_sp_right_start:
    R4 = DM(-16, I6); R8 = DM(-17, I6); R12 = R8;
    R0 = 32; DM(I7, M7) = R0;
    DK_CALL(_sp_stock_render, _sp_right_return); MODIFY(I7, 1) (NW);
    I0 = DM(-8, I6); I1 = DM(-17, I6); R1 = 0;
_sp_compare:
    R0 = 32; COMP(R1, R0); IF GE JUMP _sp_compare_history;
    R2 = DM(0, I0); R4 = DM(0, I1); JUMP _sp_compare_pair;
_sp_compare_history:
    I0 = DM(-6, I6); I1 = DM(-16, I6);
    IF NE JUMP _sp_compare_history25;
    R2 = DM(24, I0); R4 = DM(24, I1); JUMP _sp_compare_pair;
_sp_compare_history25:
    R2 = DM(25, I0); R4 = DM(25, I1);
_sp_compare_pair:
    R8 = DM(-29, I6); R0 = 0; COMP(R8, R0); IF EQ JUMP _sp_compare_finite;
    R8 = 0x7f800000; R0 = R2 AND R8; COMP(R0, R8);
    IF EQ JUMP _sp_compare_nonfinite;
    R0 = R4 AND R8; COMP(R0, R8); IF EQ JUMP _sp_compare_nonfinite;
_sp_compare_finite:
    DM(-30, I6) = R2;
    R8 = DM(-29, I6); R2 = R2 XOR R8;
    // +0 and -0 represent the same relation; all other finite bits must match.
    R8 = 0x7fffffff; R0 = R2 AND R8; R12 = R4 AND R8;
    R8 = 0; COMP(R0, R8); IF NE JUMP _sp_compare_bits;
    COMP(R12, R8); IF EQ JUMP _sp_equal;
_sp_compare_bits:
    COMP(R2, R4); IF NE JUMP _sp_mismatch;
    R8 = DM(-29, I6); R12 = 0; COMP(R8, R12); IF EQ JUMP _sp_equal;
    COMP(R0, R12); IF EQ JUMP _sp_equal;
    R0 = 32; COMP(R1, R0); IF GE JUMP _sp_count_opposite_history;
    R0 = DM(_sp_opposite_audio); R0 = R0 + 1; DM(_sp_opposite_audio) = R0;
    JUMP _sp_equal;
_sp_count_opposite_history:
    R0 = DM(_sp_opposite_history); R0 = R0 + 1; DM(_sp_opposite_history) = R0;
    JUMP _sp_equal;
_sp_mismatch:
    R0 = DM(_sp_mismatches); R8 = 0; COMP(R0, R8);
    IF NE JUMP _sp_count_mismatch;
    R0 = DM(-15, I6); DM(_sp_first_slot) = R0;
    R0 = DM(-6, I6); DM(_sp_first_state) = R0;
    R0 = DM(-7, I6); DM(_sp_first_input) = R0;
    R0 = DM(-8, I6); DM(_sp_first_output) = R0;
    DM(_sp_first_sample) = R1;
    R0 = DM(-30, I6); DM(_sp_first_left) = R0; DM(_sp_first_right) = R4;
    R0 = MODE1; DM(_sp_first_mode1) = R0;
_sp_count_mismatch:
    R0 = DM(_sp_mismatches); R0 = R0 + 1; DM(_sp_mismatches) = R0;
_sp_equal:
    MODIFY(I0, 4); MODIFY(I1, 4);
    R1 = R1 + 1; R0 = 32; COMP(R1, R0);
    IF LT JUMP _sp_compare;
    R0 = 34; COMP(R1, R0); IF LT JUMP _sp_compare;
    JUMP _sp_render_finish;
_sp_compare_nonfinite:
    R0 = DM(_sp_nonfinite); R0 = R0 + 1; DM(_sp_nonfinite) = R0;
_sp_render_finish:
    I12 = DM(-1, I6);
    R0 = DM(-13, I6); L0 = R0;
    R0 = DM(-14, I6); L1 = R0;
    R0 = DM(-9, I6); I0 = R0;
    R0 = DM(-10, I6); I1 = R0;
    R0 = DM(-25, I6); I4 = R0;
    R1 = DM(-20, I6);
    R2 = DM(-21, I6);
    R4 = DM(-22, I6);
    R8 = DM(-23, I6);
    R12 = DM(-24, I6);
    R0 = DM(-27, I6); STKYX = R0;
    R0 = DM(-26, I6); ASTATX = R0;
    R0 = DM(-19, I6);
    JUMP (M14, I12) (DB); NOP; RFRAME;
_sp_render_nonfinite:
    R0 = DM(_sp_nonfinite); R0 = R0 + 1; DM(_sp_nonfinite) = R0;
    JUMP _sp_render_fallback;
_sp_render_mode_error:
    R0 = DM(_sp_mode_errors); R0 = R0 + 1; DM(_sp_mode_errors) = R0;
_sp_render_fallback:
    R0 = DM(_sp_fallbacks); R0 = R0 + 1; DM(_sp_fallbacks) = R0;
    R0 = DM(-13, I6); L0 = R0;
    R0 = DM(-14, I6); L1 = R0;
    R0 = DM(-9, I6); I0 = R0;
    R0 = DM(-10, I6); I1 = R0;
    R0 = DM(-11, I6); I4 = R0;
    R1 = DM(-3, I6);
    R2 = DM(-4, I6);
    R4 = DM(-6, I6);
    R8 = DM(-7, I6);
    R12 = DM(-8, I6);
    R0 = DM(-28, I6); STKYX = R0;
    R0 = DM(-12, I6); ASTATX = R0;
    R0 = DM(-2, I6);
    MODIFY(I7, 32) (NW);
    JUMP _sp_stock_render;
.GLOBAL _sp_reset;
_sp_reset:
    I12 = ASTATX;
    BIT TST MODE1 0x200000;
    NOP; NOP;
    IF NOT TF JUMP _sp_reset_scalar;
    BIT CLR MODE1 0x200000;
    NOP; NOP;
    ASTATX = I12;
    BIT SET MODE1 0x200000;
    NOP; NOP;
    JUMP _sp_stock_reset;
_sp_reset_scalar:
    ASTATX = I12;
    DK_ENTER(32);
    DM(-2, I6) = R0;
    DM(-3, I6) = R1;
    DM(-4, I6) = R2;
    DM(-6, I6) = R4;
    DM(-7, I6) = R8;
    DM(-8, I6) = R12;
    R0 = I0; DM(-9, I6) = R0;
    R0 = I1; DM(-10, I6) = R0;
    R0 = I4; DM(-11, I6) = R0;
    R0 = L0; DM(-13, I6) = R0;
    R0 = L1; DM(-14, I6) = R0;
    R0 = I12; DM(-12, I6) = R0;
    R0 = STKYX; DM(-28, I6) = R0;
    R0 = 0; L0 = R0; L1 = R0;
    R0 = DM(_sp_reset_calls); R0 = R0 + 1; DM(_sp_reset_calls) = R0;
    // Raw entry pointers, before any nested stock frame. No headroom claim.
    R0 = I6; R1 = DM(_sp_low_entry_frame); R2 = 0; COMP(R1, R2);
    IF EQ JUMP _sp_reset_new_frame;
    COMP(R0, R1); IF GE JUMP _sp_reset_frame_done;
_sp_reset_new_frame:
    DM(_sp_low_entry_frame) = R0;
_sp_reset_frame_done:
    I0 = I7; MODIFY(I0, 32) (NW); R0 = I0;
    R1 = DM(_sp_low_entry_sp); R2 = 0; COMP(R1, R2);
    IF EQ JUMP _sp_reset_new_sp;
    COMP(R0, R1); IF GE JUMP _sp_reset_sp_done;
_sp_reset_new_sp:
    DM(_sp_low_entry_sp) = R0;
_sp_reset_sp_done:

    R0 = L4; R1 = 0; COMP(R0, R1); IF NE JUMP _sp_reset_fallback;
    R0 = DM(-6, I6); R1 = _sp_engine_base; R2 = 0xe088; R1 = R1 + R2; R0 = R0 - R1;
    IF LT JUMP _sp_reset_fallback;
    R1 = 0; I1 = _sp_states; I0 = _sp_audio;
_sp_reset_slot:
    R2 = 0; COMP(R0, R2);
    IF EQ JUMP _sp_reset_slot_ok;
    R2 = 112; R0 = R0 - R2;
    IF LT JUMP _sp_reset_fallback;
    MODIFY(I1, 112); MODIFY(I0, 128);
    R1 = R1 + 1; R2 = 16; COMP(R1, R2);
    IF LT JUMP _sp_reset_slot;
    JUMP _sp_reset_fallback;
_sp_reset_slot_ok:
    // Mode is private, set while paused before any accepted call; immutable thereafter.
    R0 = DM(_sp_mode); R2 = 0; COMP(R0, R2);
    IF LT JUMP _sp_reset_mode_error;
    R2 = 1; COMP(R0, R2); IF GT JUMP _sp_reset_mode_error;
    R2 = DM(_sp_active_mode); R4 = -1; COMP(R2, R4);
    IF NE JUMP _sp_reset_mode_check;
    DM(_sp_active_mode) = R0;
    R4 = MODE1; DM(_sp_numeric_mode) = R4;
_sp_reset_mode_check:
    R4 = -1; COMP(R2, R4); IF EQ JUMP _sp_reset_mode_ok;
    COMP(R0, R2); IF NE JUMP _sp_reset_mode_error;
_sp_reset_mode_ok:
    R0 = DM(_sp_mode_errors); R2 = 0; COMP(R0, R2); IF NE JUMP _sp_reset_fallback;
    DM(-15, I6) = R1;
    R0 = I1; DM(-16, I6) = R0;
    R0 = I0; DM(-17, I6) = R0;
    R0 = LSHIFT R1 BY 2; R2 = _sp_initialized; R0 = R0 + R2;
    I0 = R0; DM(-18, I6) = R0;
    R0 = DM(-13, I6); L0 = R0;
    R0 = DM(-14, I6); L1 = R0;
    R0 = DM(-9, I6); I0 = R0;
    R0 = DM(-10, I6); I1 = R0;
    R0 = DM(-11, I6); I4 = R0;
    R1 = DM(-3, I6);
    R2 = DM(-4, I6);
    R4 = DM(-6, I6);
    R8 = DM(-7, I6);
    R12 = DM(-8, I6);
    R0 = DM(-28, I6); STKYX = R0;
    R0 = DM(-12, I6); ASTATX = R0;
    R0 = DM(-2, I6);
    DK_CALL(_sp_stock_reset, _sp_reset_left_return);
    R2 = DM(-4, I6); // Reset leaves the original linked-call R2 untouched.
    DM(-19, I6) = R0;
    DM(-20, I6) = R1;
    DM(-21, I6) = R2;
    DM(-22, I6) = R4;
    DM(-23, I6) = R8;
    DM(-24, I6) = R12;
    R0 = I4; DM(-25, I6) = R0;
    R0 = ASTATX; DM(-26, I6) = R0;
    R0 = STKYX; DM(-27, I6) = R0;
    R0 = 0; L0 = R0; L1 = R0;
    I0 = DM(-18, I6); R0 = DM(0, I0); R1 = 0; COMP(R0, R1);
    IF EQ JUMP _sp_reset_finish;
    R4 = DM(-16, I6);
    DK_CALL(_sp_stock_reset, _sp_right_reset_return);
    R0 = DM(_sp_reset_mirrors); R0 = R0 + 1; DM(_sp_reset_mirrors) = R0;
_sp_reset_finish:
    I12 = DM(-1, I6);
    R0 = DM(-13, I6); L0 = R0;
    R0 = DM(-14, I6); L1 = R0;
    R0 = DM(-9, I6); I0 = R0;
    R0 = DM(-10, I6); I1 = R0;
    R0 = DM(-25, I6); I4 = R0;
    R1 = DM(-20, I6);
    R2 = DM(-21, I6);
    R4 = DM(-22, I6);
    R8 = DM(-23, I6);
    R12 = DM(-24, I6);
    R0 = DM(-27, I6); STKYX = R0;
    R0 = DM(-26, I6); ASTATX = R0;
    R0 = DM(-19, I6);
    JUMP (M14, I12) (DB); NOP; RFRAME;
_sp_reset_mode_error:
    R0 = DM(_sp_mode_errors); R0 = R0 + 1; DM(_sp_mode_errors) = R0;
_sp_reset_fallback:
    R0 = DM(_sp_fallbacks); R0 = R0 + 1; DM(_sp_fallbacks) = R0;
    R0 = DM(-13, I6); L0 = R0;
    R0 = DM(-14, I6); L1 = R0;
    R0 = DM(-9, I6); I0 = R0;
    R0 = DM(-10, I6); I1 = R0;
    R0 = DM(-11, I6); I4 = R0;
    R1 = DM(-3, I6);
    R2 = DM(-4, I6);
    R4 = DM(-6, I6);
    R8 = DM(-7, I6);
    R12 = DM(-8, I6);
    R0 = DM(-28, I6); STKYX = R0;
    R0 = DM(-12, I6); ASTATX = R0;
    R0 = DM(-2, I6);
    MODIFY(I7, 32) (NW);
    JUMP _sp_stock_reset;
.SECTION/DM seg_dmda;
.PKDATA _sp_states, 1792;
.PKDATA _sp_audio, 2048;
.PKDATA _sp_initialized, 64;
.GLOBAL _sp_render_calls; .VAR _sp_render_calls = 0;
.GLOBAL _sp_shadow_calls; .VAR _sp_shadow_calls = 0;
.GLOBAL _sp_reset_calls; .VAR _sp_reset_calls = 0;
.GLOBAL _sp_reset_mirrors; .VAR _sp_reset_mirrors = 0;
.GLOBAL _sp_mismatches; .VAR _sp_mismatches = 0;
.GLOBAL _sp_nonzero_input; .VAR _sp_nonzero_input = 0;
.GLOBAL _sp_nonidentity_coeff; .VAR _sp_nonidentity_coeff = 0;
.GLOBAL _sp_fallbacks; .VAR _sp_fallbacks = 0;
.GLOBAL _sp_first_slot; .VAR _sp_first_slot = 0;
.GLOBAL _sp_first_state; .VAR _sp_first_state = 0;
.GLOBAL _sp_first_input; .VAR _sp_first_input = 0;
.GLOBAL _sp_first_output; .VAR _sp_first_output = 0;
.GLOBAL _sp_first_sample; .VAR _sp_first_sample = 0;
.GLOBAL _sp_first_left; .VAR _sp_first_left = 0;
.GLOBAL _sp_first_right; .VAR _sp_first_right = 0;
.GLOBAL _sp_first_mode1; .VAR _sp_first_mode1 = 0;
.GLOBAL _sp_low_entry_frame; .VAR _sp_low_entry_frame = 0;
.GLOBAL _sp_low_entry_sp; .VAR _sp_low_entry_sp = 0;
.GLOBAL _sp_coeff_slot; .VAR _sp_coeff_slot = 0;
.GLOBAL _sp_coeff21; .VAR _sp_coeff21 = 0;
.GLOBAL _sp_coeff22; .VAR _sp_coeff22 = 0;
.GLOBAL _sp_coeff23; .VAR _sp_coeff23 = 0;
// Raw mode may be configured only before first accepted render/reset.
.GLOBAL _sp_mode; .VAR _sp_mode = 0;
.GLOBAL _sp_active_mode; .VAR _sp_active_mode = 0xffffffff;
.GLOBAL _sp_mode_errors; .VAR _sp_mode_errors = 0;
// Polarity-only finite trajectory refusal; reload to start a fresh proof.
// Identity mode retains its earlier exceptional-value comparison behavior.
.GLOBAL _sp_nonfinite; .VAR _sp_nonfinite = 0;
.GLOBAL _sp_opposite_audio; .VAR _sp_opposite_audio = 0;
.GLOBAL _sp_opposite_history; .VAR _sp_opposite_history = 0;

.GLOBAL _sp_numeric_mode; .VAR _sp_numeric_mode = 0;
