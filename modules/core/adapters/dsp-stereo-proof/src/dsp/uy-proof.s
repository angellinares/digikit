#include "dsp/call.inc"
// Private DN2 1.12 U/Y identity/polarity observer, shared by BOTH engine placements. Left state/output stay authoritative.
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
.EXTERN _uy_stock_render;
.EXTERN _uy_stock_reset;
.EXTERN _uy_engine_base;
.GLOBAL _uy_render;
_uy_render:
    I12 = ASTATX;
    BIT TST MODE1 0x200000;
    NOP; NOP;
    IF NOT TF JUMP _uy_render_scalar;
    BIT CLR MODE1 0x200000;
    NOP; NOP;
    ASTATX = I12;
    BIT SET MODE1 0x200000;
    NOP; NOP;
    JUMP _uy_stock_render;
_uy_render_scalar:
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
    R0 = DM(_uy_render_calls); R0 = R0 + 1; DM(_uy_render_calls) = R0;
    // Raw entry pointers, before any nested stock frame. No headroom claim.
    R0 = I6; R1 = DM(_uy_low_entry_frame); R2 = 0; COMP(R1, R2);
    IF EQ JUMP _uy_render_new_frame;
    COMP(R0, R1); IF GE JUMP _uy_render_frame_done;
_uy_render_new_frame:
    DM(_uy_low_entry_frame) = R0;
_uy_render_frame_done:
    I0 = I7; MODIFY(I0, 32) (NW); R0 = I0;
    R1 = DM(_uy_low_entry_sp); R2 = 0; COMP(R1, R2);
    IF EQ JUMP _uy_render_new_sp;
    COMP(R0, R1); IF GE JUMP _uy_render_uy_done;
_uy_render_new_sp:
    DM(_uy_low_entry_sp) = R0;
_uy_render_uy_done:

    R0 = DM(1, I6); R1 = 32; COMP(R0, R1);
    IF NE JUMP _uy_render_fallback;
    // Private right pointers require linear stock state/input/output DAGs.
    R0 = L3; R1 = L4; R0 = R0 OR R1; R1 = L5; R0 = R0 OR R1;
    R1 = 0; COMP(R0, R1); IF NE JUMP _uy_render_fallback;
    R0 = DM(-6, I6); R1 = _uy_engine_base; R2 = 0x12148; R1 = R1 + R2; R0 = R0 - R1;
    IF LT JUMP _uy_render_fallback;
    R1 = 0; I1 = _uy_states; I0 = _uy_audio;
_uy_render_slot:
    R2 = 0; COMP(R0, R2);
    IF EQ JUMP _uy_render_slot_ok;
    R2 = 32; R0 = R0 - R2;
    IF LT JUMP _uy_render_fallback;
    MODIFY(I1, 32); MODIFY(I0, 128);
    R1 = R1 + 1; R2 = 16; COMP(R1, R2);
    IF LT JUMP _uy_render_slot;
    JUMP _uy_render_fallback;
_uy_render_slot_ok:
    // Mode is private, set while paused before any accepted call; immutable thereafter.
    R0 = DM(_sp_mode); R2 = 0; COMP(R0, R2);
    IF LT JUMP _uy_render_mode_error;
    R2 = 1; COMP(R0, R2); IF GT JUMP _uy_render_mode_error;
    R2 = DM(_sp_active_mode); R4 = -1; COMP(R2, R4);
    IF NE JUMP _uy_render_shared_check;
    DM(_sp_active_mode) = R0;
    R4 = MODE1; DM(_sp_numeric_mode) = R4;
    JUMP _uy_render_shared_ok;
_uy_render_shared_check:
    COMP(R0, R2); IF NE JUMP _uy_render_mode_error;
_uy_render_shared_ok:
    R2 = DM(_sp_mode_errors); R4 = 0; COMP(R2, R4);
    IF NE JUMP _uy_render_fallback;
    R2 = DM(_uy_active_mode); R4 = -1; COMP(R2, R4);
    IF NE JUMP _uy_render_mode_check;
    DM(_uy_active_mode) = R0;
    R4 = MODE1; DM(_uy_numeric_mode) = R4;
_uy_render_mode_check:
    R4 = -1; COMP(R2, R4); IF EQ JUMP _uy_render_mode_ok;
    COMP(R0, R2); IF NE JUMP _uy_render_mode_error;
_uy_render_mode_ok:
    R0 = LSHIFT R0 BY 31; DM(-29, I6) = R0;
    R0 = DM(_uy_mode_errors); R2 = 0; COMP(R0, R2); IF NE JUMP _uy_render_fallback;
    R0 = DM(_uy_nonfinite); R2 = 0; COMP(R0, R2);
    IF NE JUMP _uy_render_fallback;
    DM(-15, I6) = R1;
    R0 = I1; DM(-16, I6) = R0;
    R0 = I0; DM(-17, I6) = R0;
    R0 = LSHIFT R1 BY 2; R2 = _uy_initialized; R0 = R0 + R2;
    I0 = R0; DM(-18, I6) = R0;
    R0 = DM(0, I0); R1 = 0; COMP(R0, R1);
    IF NE JUMP _uy_render_coeff;
    I0 = DM(-6, I6); I1 = DM(-16, I6); R1 = 8;
_uy_clone:
    R0 = DM(0, I0); DM(0, I1) = R0;
    MODIFY(I0, 4); MODIFY(I1, 4);
    R1 = R1 - 1; IF NE JUMP _uy_clone;
    // Only live audio histories6/7 participate in the odd recurrence.
    I1 = DM(-16, I6); R2 = DM(-29, I6);
    R0 = DM(6, I1); R0 = R0 XOR R2; DM(6, I1) = R0;
    R0 = DM(7, I1); R0 = R0 XOR R2; DM(7, I1) = R0;
    I0 = DM(-18, I6); R0 = 1; DM(0, I0) = R0;
_uy_render_coeff:
    I0 = DM(-6, I6); I1 = DM(-16, I6);
    R0 = DM(2, I0); DM(2, I1) = R0;
    R0 = DM(3, I0); DM(3, I1) = R0;
    R0 = DM(4, I0); DM(4, I1) = R0;
    R0 = DM(5, I0); DM(5, I1) = R0;
    R0 = DM(2, I0); R2 = 0x7f800000; R0 = R0 AND R2;
    COMP(R0, R2); IF EQ JUMP _uy_render_nonfinite;
    R0 = DM(3, I0); R2 = 0x7f800000; R0 = R0 AND R2;
    COMP(R0, R2); IF EQ JUMP _uy_render_nonfinite;
    R0 = DM(5, I0); R2 = 0x7f800000; R0 = R0 AND R2;
    COMP(R0, R2); IF EQ JUMP _uy_render_nonfinite;
    R0 = DM(4, I0); R2 = 0x7f800000; R0 = R0 AND R2;
    COMP(R0, R2); IF EQ JUMP _uy_render_nonfinite;
    R0 = DM(6, I0); R2 = 0x7f800000; R0 = R0 AND R2;
    COMP(R0, R2); IF EQ JUMP _uy_render_nonfinite;
    R0 = DM(7, I0); R2 = 0x7f800000; R0 = R0 AND R2;
    COMP(R0, R2); IF EQ JUMP _uy_render_nonfinite;
    R0 = DM(6, I1); R2 = 0x7f800000; R0 = R0 AND R2;
    COMP(R0, R2); IF EQ JUMP _uy_render_nonfinite;
    R0 = DM(7, I1); R2 = 0x7f800000; R0 = R0 AND R2;
    COMP(R0, R2); IF EQ JUMP _uy_render_nonfinite;
_uy_finite_state_done:
    // Capture every occurrence; never reset histories between early/late placement.
    R0 = DM(_uy_coeff_blocks); R1 = 0; COMP(R0, R1);
    IF NE JUMP _uy_coeff_count;
    R0 = DM(-15, I6); DM(_uy_coeff_slot) = R0;
    R0 = DM(2, I1); DM(_uy_coeff2) = R0;
    R0 = DM(3, I1); DM(_uy_coeff3) = R0;
    R0 = DM(4, I1); DM(_uy_coeff4) = R0;
    R0 = DM(5, I1); DM(_uy_coeff5) = R0;
_uy_coeff_count:
    R0 = DM(_uy_coeff_blocks); R0 = R0 + 1; DM(_uy_coeff_blocks) = R0;
_uy_copy_input:
    I0 = DM(-7, I6); I1 = DM(-17, I6); R1 = 32;
_uy_input_loop:
    R0 = DM(0, I0);
    R2 = 0x7f800000; R4 = R0 AND R2;
    COMP(R4, R2); IF EQ JUMP _uy_render_nonfinite;
_uy_input_finite:
    R2 = DM(-29, I6); R0 = R0 XOR R2; DM(0, I1) = R0;
    R2 = 0x7fffffff; R2 = R0 AND R2;
    R4 = 0; COMP(R2, R4); IF EQ JUMP _uy_input_zero;
    R0 = DM(_uy_nonzero_input); R0 = R0 + 1; DM(_uy_nonzero_input) = R0;
_uy_input_zero:
    MODIFY(I0, 4); MODIFY(I1, 4);
    R1 = R1 - 1; IF NE JUMP _uy_input_loop;
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
    DK_CALL(_uy_stock_render, _uy_render_left_return);
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
    R0 = DM(_uy_shadow_calls); R0 = R0 + 1; DM(_uy_shadow_calls) = R0;
_uy_right_start:
    R4 = DM(-16, I6); R8 = DM(-17, I6); R12 = R8;
    R0 = 32; DM(I7, M7) = R0;
    DK_CALL(_uy_stock_render, _uy_right_return); MODIFY(I7, 1) (NW);
    I0 = DM(-8, I6); I1 = DM(-17, I6); R1 = 0;
_uy_compare:
    R0 = 32; COMP(R1, R0); IF GE JUMP _uy_compare_history;
    R2 = DM(0, I0); R4 = DM(0, I1); JUMP _uy_compare_pair;
_uy_compare_history:
    I0 = DM(-6, I6); I1 = DM(-16, I6);
    IF NE JUMP _uy_compare_history25;
    R2 = DM(6, I0); R4 = DM(6, I1); JUMP _uy_compare_pair;
_uy_compare_history25:
    R2 = DM(7, I0); R4 = DM(7, I1);
_uy_compare_pair:
    R8 = 0x7f800000; R0 = R2 AND R8; COMP(R0, R8);
    IF EQ JUMP _uy_compare_nonfinite;
    R0 = R4 AND R8; COMP(R0, R8); IF EQ JUMP _uy_compare_nonfinite;
_uy_compare_finite:
    DM(-30, I6) = R2;
    R8 = DM(-29, I6); R2 = R2 XOR R8;
    // +0 and -0 represent the same relation; all other finite bits must match.
    R8 = 0x7fffffff; R0 = R2 AND R8; R12 = R4 AND R8;
    R8 = 0; COMP(R0, R8); IF NE JUMP _uy_compare_bits;
    COMP(R12, R8); IF EQ JUMP _uy_equal;
_uy_compare_bits:
    COMP(R2, R4); IF NE JUMP _uy_mismatch;
    R8 = DM(-29, I6); R12 = 0; COMP(R8, R12); IF EQ JUMP _uy_equal;
    COMP(R0, R12); IF EQ JUMP _uy_equal;
    R0 = 32; COMP(R1, R0); IF GE JUMP _uy_count_opposite_history;
    R0 = DM(_uy_opposite_audio); R0 = R0 + 1; DM(_uy_opposite_audio) = R0;
    JUMP _uy_equal;
_uy_count_opposite_history:
    R0 = DM(_uy_opposite_history); R0 = R0 + 1; DM(_uy_opposite_history) = R0;
    JUMP _uy_equal;
_uy_mismatch:
    R0 = DM(_uy_mismatches); R8 = 0; COMP(R0, R8);
    IF NE JUMP _uy_count_mismatch;
    R0 = DM(-15, I6); DM(_uy_first_slot) = R0;
    R0 = DM(-6, I6); DM(_uy_first_state) = R0;
    R0 = DM(-7, I6); DM(_uy_first_input) = R0;
    R0 = DM(-8, I6); DM(_uy_first_output) = R0;
    DM(_uy_first_sample) = R1;
    R0 = DM(-30, I6); DM(_uy_first_left) = R0; DM(_uy_first_right) = R4;
    R0 = MODE1; DM(_uy_first_mode1) = R0;
_uy_count_mismatch:
    R0 = DM(_uy_mismatches); R0 = R0 + 1; DM(_uy_mismatches) = R0;
_uy_equal:
    MODIFY(I0, 4); MODIFY(I1, 4);
    R1 = R1 + 1; R0 = 32; COMP(R1, R0);
    IF LT JUMP _uy_compare;
    R0 = 34; COMP(R1, R0); IF LT JUMP _uy_compare;
    JUMP _uy_render_finish;
_uy_compare_nonfinite:
    R0 = DM(_uy_nonfinite); R0 = R0 + 1; DM(_uy_nonfinite) = R0;
_uy_render_finish:
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
_uy_render_nonfinite:
    R0 = DM(_uy_nonfinite); R0 = R0 + 1; DM(_uy_nonfinite) = R0;
    JUMP _uy_render_fallback;
_uy_render_mode_error:
    R0 = DM(_sp_mode_errors); R0 = R0 + 1; DM(_sp_mode_errors) = R0;
    R0 = DM(_uy_mode_errors); R0 = R0 + 1; DM(_uy_mode_errors) = R0;
_uy_render_fallback:
    R0 = DM(_uy_fallbacks); R0 = R0 + 1; DM(_uy_fallbacks) = R0;
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
    JUMP _uy_stock_render;
.GLOBAL _uy_reset;
_uy_reset:
    I12 = ASTATX;
    BIT TST MODE1 0x200000;
    NOP; NOP;
    IF NOT TF JUMP _uy_reset_scalar;
    BIT CLR MODE1 0x200000;
    NOP; NOP;
    ASTATX = I12;
    BIT SET MODE1 0x200000;
    NOP; NOP;
    JUMP _uy_stock_reset;
_uy_reset_scalar:
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
    R0 = DM(_uy_reset_calls); R0 = R0 + 1; DM(_uy_reset_calls) = R0;
    // Raw entry pointers, before any nested stock frame. No headroom claim.
    R0 = I6; R1 = DM(_uy_low_entry_frame); R2 = 0; COMP(R1, R2);
    IF EQ JUMP _uy_reset_new_frame;
    COMP(R0, R1); IF GE JUMP _uy_reset_frame_done;
_uy_reset_new_frame:
    DM(_uy_low_entry_frame) = R0;
_uy_reset_frame_done:
    I0 = I7; MODIFY(I0, 32) (NW); R0 = I0;
    R1 = DM(_uy_low_entry_sp); R2 = 0; COMP(R1, R2);
    IF EQ JUMP _uy_reset_new_sp;
    COMP(R0, R1); IF GE JUMP _uy_reset_uy_done;
_uy_reset_new_sp:
    DM(_uy_low_entry_sp) = R0;
_uy_reset_uy_done:

    R0 = L4; R1 = 0; COMP(R0, R1); IF NE JUMP _uy_reset_fallback;
    R0 = DM(-6, I6); R1 = _uy_engine_base; R2 = 0x12148; R1 = R1 + R2; R0 = R0 - R1;
    IF LT JUMP _uy_reset_fallback;
    R1 = 0; I1 = _uy_states; I0 = _uy_audio;
_uy_reset_slot:
    R2 = 0; COMP(R0, R2);
    IF EQ JUMP _uy_reset_slot_ok;
    R2 = 32; R0 = R0 - R2;
    IF LT JUMP _uy_reset_fallback;
    MODIFY(I1, 32); MODIFY(I0, 128);
    R1 = R1 + 1; R2 = 16; COMP(R1, R2);
    IF LT JUMP _uy_reset_slot;
    JUMP _uy_reset_fallback;
_uy_reset_slot_ok:
    // Mode is private, set while paused before any accepted call; immutable thereafter.
    R0 = DM(_sp_mode); R2 = 0; COMP(R0, R2);
    IF LT JUMP _uy_reset_mode_error;
    R2 = 1; COMP(R0, R2); IF GT JUMP _uy_reset_mode_error;
    R2 = DM(_sp_active_mode); R4 = -1; COMP(R2, R4);
    IF NE JUMP _uy_reset_shared_check;
    DM(_sp_active_mode) = R0;
    R4 = MODE1; DM(_sp_numeric_mode) = R4;
    JUMP _uy_reset_shared_ok;
_uy_reset_shared_check:
    COMP(R0, R2); IF NE JUMP _uy_reset_mode_error;
_uy_reset_shared_ok:
    R2 = DM(_sp_mode_errors); R4 = 0; COMP(R2, R4);
    IF NE JUMP _uy_reset_fallback;
    R2 = DM(_uy_active_mode); R4 = -1; COMP(R2, R4);
    IF NE JUMP _uy_reset_mode_check;
    DM(_uy_active_mode) = R0;
    R4 = MODE1; DM(_uy_numeric_mode) = R4;
_uy_reset_mode_check:
    R4 = -1; COMP(R2, R4); IF EQ JUMP _uy_reset_mode_ok;
    COMP(R0, R2); IF NE JUMP _uy_reset_mode_error;
_uy_reset_mode_ok:
    R0 = DM(_uy_mode_errors); R2 = 0; COMP(R0, R2); IF NE JUMP _uy_reset_fallback;
    DM(-15, I6) = R1;
    R0 = I1; DM(-16, I6) = R0;
    R0 = I0; DM(-17, I6) = R0;
    R0 = LSHIFT R1 BY 2; R2 = _uy_initialized; R0 = R0 + R2;
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
    DK_CALL(_uy_stock_reset, _uy_reset_left_return);
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
    IF EQ JUMP _uy_reset_finish;
    R4 = DM(-16, I6);
    DK_CALL(_uy_stock_reset, _uy_right_reset_return);
    R0 = DM(_uy_reset_mirrors); R0 = R0 + 1; DM(_uy_reset_mirrors) = R0;
_uy_reset_finish:
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
_uy_reset_mode_error:
    R0 = DM(_sp_mode_errors); R0 = R0 + 1; DM(_sp_mode_errors) = R0;
    R0 = DM(_uy_mode_errors); R0 = R0 + 1; DM(_uy_mode_errors) = R0;
_uy_reset_fallback:
    R0 = DM(_uy_fallbacks); R0 = R0 + 1; DM(_uy_fallbacks) = R0;
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
    JUMP _uy_stock_reset;
.SECTION/DM seg_dmda;
.PKDATA _uy_states, 512;
.PKDATA _uy_audio, 2048;
.PKDATA _uy_initialized, 64;
.GLOBAL _uy_render_calls; .VAR _uy_render_calls = 0;
.GLOBAL _uy_shadow_calls; .VAR _uy_shadow_calls = 0;
.GLOBAL _uy_reset_calls; .VAR _uy_reset_calls = 0;
.GLOBAL _uy_reset_mirrors; .VAR _uy_reset_mirrors = 0;
.GLOBAL _uy_mismatches; .VAR _uy_mismatches = 0;
.GLOBAL _uy_nonzero_input; .VAR _uy_nonzero_input = 0;
.GLOBAL _uy_coeff_blocks; .VAR _uy_coeff_blocks = 0;
.GLOBAL _uy_fallbacks; .VAR _uy_fallbacks = 0;
.GLOBAL _uy_first_slot; .VAR _uy_first_slot = 0;
.GLOBAL _uy_first_state; .VAR _uy_first_state = 0;
.GLOBAL _uy_first_input; .VAR _uy_first_input = 0;
.GLOBAL _uy_first_output; .VAR _uy_first_output = 0;
.GLOBAL _uy_first_sample; .VAR _uy_first_sample = 0;
.GLOBAL _uy_first_left; .VAR _uy_first_left = 0;
.GLOBAL _uy_first_right; .VAR _uy_first_right = 0;
.GLOBAL _uy_first_mode1; .VAR _uy_first_mode1 = 0;
.GLOBAL _uy_low_entry_frame; .VAR _uy_low_entry_frame = 0;
.GLOBAL _uy_low_entry_sp; .VAR _uy_low_entry_sp = 0;
.GLOBAL _uy_coeff_slot; .VAR _uy_coeff_slot = 0;
.GLOBAL _uy_coeff2; .VAR _uy_coeff2 = 0;
.GLOBAL _uy_coeff3; .VAR _uy_coeff3 = 0;
.GLOBAL _uy_coeff4; .VAR _uy_coeff4 = 0;
// Raw mode may be configured only before first accepted render/reset.
.EXTERN _sp_mode;
.EXTERN _sp_active_mode;
.EXTERN _sp_mode_errors;
.EXTERN _sp_numeric_mode;
.GLOBAL _uy_active_mode; .VAR _uy_active_mode = 0xffffffff;
.GLOBAL _uy_mode_errors; .VAR _uy_mode_errors = 0;
// Finite-only U/Y trajectory refusal in BOTH modes; reload for a fresh proof.
.GLOBAL _uy_nonfinite; .VAR _uy_nonfinite = 0;
.GLOBAL _uy_opposite_audio; .VAR _uy_opposite_audio = 0;
.GLOBAL _uy_opposite_history; .VAR _uy_opposite_history = 0;

.GLOBAL _uy_numeric_mode; .VAR _uy_numeric_mode = 0;

.GLOBAL _uy_coeff5; .VAR _uy_coeff5 = 0;
