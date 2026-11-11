#include "dk_machine.inc"
#include "dsp/contract/call.inc"
#include "dsp/sample/float.inc"
#include "dsp/sample/pcm16.inc"
#include "dsp/sample/bank.inc"
.SECTION/PM seg_pmco;
.GLOBAL _lp_play_note_on;
.GLOBAL _lp_play_render;
.EXTERN _lp_close_a_open;
.EXTERN _lp_close_a_ready;
.EXTERN _lp_close_a_closed;
.EXTERN _lp_close_a_count;
.EXTERN _lp_close_a_fault;
.EXTERN _lp_close_a_epoch;
.EXTERN _lp_reader_a_source;
.EXTERN _lp_reader_a_frames;
.EXTERN _lp_close_b_open;
.EXTERN _lp_close_b_ready;
.EXTERN _lp_close_b_closed;
.EXTERN _lp_close_b_count;
.EXTERN _lp_close_b_fault;
.EXTERN _lp_close_b_epoch;
.EXTERN _lp_reader_b_source;
.EXTERN _lp_reader_b_frames;
// Voice: six SDK PCM cursor words, epoch+6, source+7, cached track+8.
_lp_play_note_on:
    I3 = R4;
    I2 = R8;
    NOP; NOP; NOP;
    R6 = DM(DK_IN_W_TRACK, I2);
    DM(8, I3) = R6;
    R0 = 0;
    DM(0, I3) = R0; DM(1, I3) = R0; DM(2, I3) = R0;
    DM(3, I3) = R0; DM(6, I3) = R0; DM(7, I3) = R0;
    R0 = 65536; DM(4, I3) = R0;
    R0 = 1; DM(5, I3) = R0;
    R0 = 0; COMP(R6, R0); IF EQ JUMP _lp_reader_a_note;
    R0 = 1; COMP(R6, R0); IF EQ JUMP _lp_reader_b_note;
    JUMP _lp_reader_note_denied;
_lp_play_render:
    I3 = R4;
    I2 = R8;
    NOP; NOP; NOP;
    R6 = DM(DK_IN_W_TRACK, I2);
    R0 = 0; COMP(R6, R0); IF EQ JUMP _lp_reader_a_render;
    R0 = 1; COMP(R6, R0); IF EQ JUMP _lp_reader_b_render;
    JUMP _lp_reader_silent;
_lp_reader_note_denied:
    R0 = 1;
    JUMP (M13, I13);
_lp_reader_silent:
    R1 = DM(DK_IN_W_OUT, I2);
    I2 = R1;
    I4 = R1;
    R0 = 0;
    R2 = 32;
    NOP; NOP; NOP;
_lp_reader_zero:
    DM(I2, M6) = R0;
    R2 = R2 - 1;
    R2 = PASS R2;
    IF GT JUMP _lp_reader_zero;
    R0 = 1; DM(5, I3) = R0;
    R0 = 0; COMP(R6, R0); IF EQ JUMP _lp_reader_a_snapshot_silent;
    R0 = 1; COMP(R6, R0); IF EQ JUMP _lp_reader_b_snapshot_silent;
    JUMP _lp_reader_silent_return;
_lp_reader_a_snapshot_silent:
    // Private callback-boundary observation, before stock stages mutate output.
    R0 = DM(_lp_reader_a_snapshot_seq); R0 = R0 + 1;
    DM(_lp_reader_a_snapshot_seq) = R0;
    I0 = _lp_reader_a_snapshot;
    R2 = 32;
    NOP; NOP; NOP;
_lp_reader_a_snapshot_silent_copy:
    R1 = DM(I4, M6); DM(I0, M6) = R1;
    R2 = R2 - 1; R2 = PASS R2; IF GT JUMP _lp_reader_a_snapshot_silent_copy;
    R0 = DM(_lp_reader_a_snapshot_seq); R0 = R0 + 1;
    DM(_lp_reader_a_snapshot_seq) = R0;
    JUMP _lp_reader_silent_return;
_lp_reader_b_snapshot_silent:
    // Private callback-boundary observation, before stock stages mutate output.
    R0 = DM(_lp_reader_b_snapshot_seq); R0 = R0 + 1;
    DM(_lp_reader_b_snapshot_seq) = R0;
    I0 = _lp_reader_b_snapshot;
    R2 = 32;
    NOP; NOP; NOP;
_lp_reader_b_snapshot_silent_copy:
    R1 = DM(I4, M6); DM(I0, M6) = R1;
    R2 = R2 - 1; R2 = PASS R2; IF GT JUMP _lp_reader_b_snapshot_silent_copy;
    R0 = DM(_lp_reader_b_snapshot_seq); R0 = R0 + 1;
    DM(_lp_reader_b_snapshot_seq) = R0;
_lp_reader_silent_return:
    R0 = 1;
    JUMP (M13, I13);
_lp_reader_a_note:
    R0 = I3; DM(_lp_reader_a_state) = R0;
    R0 = DM(_lp_reader_a_notes); R0 = R0 + 1; DM(_lp_reader_a_notes) = R0;
    DK_SAMPLE_BANK_LOCK(R2);
    DK_SAMPLE_BANK_CHECK_ADMISSION(_lp_close_a_open, _lp_close_a_closed, _lp_close_a_count, R0, R1, _lp_reader_a_note_denied, _lp_reader_a_note_fault);
    DK_SAMPLE_BANK_LATCH_EPOCH(_lp_close_a_ready, _lp_close_a_epoch, I3, 6, R1, R0, _lp_reader_a_note_denied);
    DK_SAMPLE_BANK_ACQUIRE(_lp_close_a_count, R0);
    DK_SAMPLE_BANK_UNLOCK(R2);
.GLOBAL _lp_reader_a_note_acquired;
_lp_reader_a_note_acquired:
    R0 = DM(_lp_reader_a_source); DM(7, I3) = R0;
    R0 = DM(_lp_reader_a_frames); DM(1, I3) = R0;
    R1 = 65536; DM(4, I3) = R1;
    I5 = R12;
    NOP; NOP; NOP;
    R1 = DM(0, I5); R7 = 128; COMP(R1, R7);
    IF LT JUMP _lp_reader_a_forward;
    R0 = R0 - 1; DM(2, I3) = R0;
    R1 = -65536; DM(4, I3) = R1;
_lp_reader_a_forward:
    R0 = 0; DM(5, I3) = R0;
    DK_SAMPLE_BANK_LOCK(R2);
    DK_SAMPLE_BANK_RELEASE(_lp_close_a_count, R0, _lp_reader_a_note_release_fault, _lp_reader_a_note_released);
_lp_reader_a_note_release_fault:
    DK_SAMPLE_BANK_FAIL_CLOSED(_lp_close_a_fault, _lp_close_a_closed, _lp_close_a_open, _lp_close_a_ready, R0);
    DK_SAMPLE_BANK_POISON_COUNT(_lp_close_a_count, R0);
_lp_reader_a_note_released:
    DK_SAMPLE_BANK_UNLOCK(R2);
    R0 = 0;
    JUMP (M13, I13);
_lp_reader_a_note_fault:
    DK_SAMPLE_BANK_FAIL_CLOSED(_lp_close_a_fault, _lp_close_a_closed, _lp_close_a_open, _lp_close_a_ready, R0);
    DK_SAMPLE_BANK_POISON_COUNT(_lp_close_a_count, R0);
_lp_reader_a_note_denied:
    DK_SAMPLE_BANK_UNLOCK(R2);
    JUMP _lp_reader_note_denied;
_lp_reader_a_render:
    R0 = I3; DM(_lp_reader_a_state) = R0;
    R0 = DM(_lp_reader_a_blocks); R0 = R0 + 1; DM(_lp_reader_a_blocks) = R0;
    DK_SAMPLE_BANK_LOCK(R2);
    // Cached ownership joins epoch/admission/count in the same masked scope.
    R0 = DM(8, I3); COMP(R0, R6); IF NE JUMP _lp_reader_a_render_denied;
    DK_SAMPLE_BANK_CHECK_ADMISSION(_lp_close_a_open, _lp_close_a_closed, _lp_close_a_count, R0, R1, _lp_reader_a_render_denied, _lp_reader_a_render_fault);
    R0 = DM(_lp_close_a_ready); R1 = 1; COMP(R0, R1); IF NE JUMP _lp_reader_a_render_denied;
    DK_SAMPLE_BANK_CHECK_EPOCH(_lp_close_a_epoch, I3, 6, R0, R1, _lp_reader_a_render_denied);
    DK_SAMPLE_BANK_ACQUIRE(_lp_close_a_count, R0);
    DK_SAMPLE_BANK_UNLOCK(R2);
.GLOBAL _lp_reader_a_render_acquired;
_lp_reader_a_render_acquired:
    R0 = DM(_lp_reader_a_reads); R0 = R0 + 1; DM(_lp_reader_a_reads) = R0;
    R0 = DM(DK_IN_W_OUT, I2); DM(_lp_reader_a_output) = R0;
    R3 = I12; R5 = I13;
    MODIFY(I7, -1) (NW);
    R4 = DM(7, I3);
    R8 = DM(DK_IN_W_OUT, I2);
    R12 = I3;
    DK_CALL(_dk_sample_pcm16_linear, _lp_reader_a_after_reader);
    R0 = DM(DK_IN_W_OUT, I2); I4 = R0;
    // Private callback-boundary observation, before stock stages mutate output.
    R0 = DM(_lp_reader_a_snapshot_seq); R0 = R0 + 1;
    DM(_lp_reader_a_snapshot_seq) = R0;
    I0 = _lp_reader_a_snapshot;
    R2 = 32;
    NOP; NOP; NOP;
_lp_reader_a_snapshot_read_copy:
    R1 = DM(I4, M6); DM(I0, M6) = R1;
    R2 = R2 - 1; R2 = PASS R2; IF GT JUMP _lp_reader_a_snapshot_read_copy;
    R0 = DM(_lp_reader_a_snapshot_seq); R0 = R0 + 1;
    DM(_lp_reader_a_snapshot_seq) = R0;
    MODIFY(I7, 1) (NW);
    I12 = R3; I13 = R5;
    DK_SAMPLE_BANK_LOCK(R2);
    DK_SAMPLE_BANK_RELEASE(_lp_close_a_count, R0, _lp_reader_a_render_release_fault, _lp_reader_a_render_released);
_lp_reader_a_render_release_fault:
    DK_SAMPLE_BANK_FAIL_CLOSED(_lp_close_a_fault, _lp_close_a_closed, _lp_close_a_open, _lp_close_a_ready, R0);
    DK_SAMPLE_BANK_POISON_COUNT(_lp_close_a_count, R0);
_lp_reader_a_render_released:
    DK_SAMPLE_BANK_UNLOCK(R2);
    R0 = DM(5, I3);
    JUMP (M13, I13);
_lp_reader_a_render_fault:
    DK_SAMPLE_BANK_FAIL_CLOSED(_lp_close_a_fault, _lp_close_a_closed, _lp_close_a_open, _lp_close_a_ready, R0);
    DK_SAMPLE_BANK_POISON_COUNT(_lp_close_a_count, R0);
_lp_reader_a_render_denied:
    DK_SAMPLE_BANK_UNLOCK(R2);
    JUMP _lp_reader_silent;
_lp_reader_b_note:
    R0 = I3; DM(_lp_reader_b_state) = R0;
    R0 = DM(_lp_reader_b_notes); R0 = R0 + 1; DM(_lp_reader_b_notes) = R0;
    DK_SAMPLE_BANK_LOCK(R2);
    DK_SAMPLE_BANK_CHECK_ADMISSION(_lp_close_b_open, _lp_close_b_closed, _lp_close_b_count, R0, R1, _lp_reader_b_note_denied, _lp_reader_b_note_fault);
    DK_SAMPLE_BANK_LATCH_EPOCH(_lp_close_b_ready, _lp_close_b_epoch, I3, 6, R1, R0, _lp_reader_b_note_denied);
    DK_SAMPLE_BANK_ACQUIRE(_lp_close_b_count, R0);
    DK_SAMPLE_BANK_UNLOCK(R2);
.GLOBAL _lp_reader_b_note_acquired;
_lp_reader_b_note_acquired:
    R0 = DM(_lp_reader_b_source); DM(7, I3) = R0;
    R0 = DM(_lp_reader_b_frames); DM(1, I3) = R0;
    R1 = 1024; DM(4, I3) = R1;
    I5 = R12;
    NOP; NOP; NOP;
    R1 = DM(0, I5); R7 = 128; COMP(R1, R7);
    IF LT JUMP _lp_reader_b_forward;
    R0 = R0 - 1; DM(2, I3) = R0;
    R1 = -1024; DM(4, I3) = R1;
_lp_reader_b_forward:
    R0 = 0; DM(5, I3) = R0;
    DK_SAMPLE_BANK_LOCK(R2);
    DK_SAMPLE_BANK_RELEASE(_lp_close_b_count, R0, _lp_reader_b_note_release_fault, _lp_reader_b_note_released);
_lp_reader_b_note_release_fault:
    DK_SAMPLE_BANK_FAIL_CLOSED(_lp_close_b_fault, _lp_close_b_closed, _lp_close_b_open, _lp_close_b_ready, R0);
    DK_SAMPLE_BANK_POISON_COUNT(_lp_close_b_count, R0);
_lp_reader_b_note_released:
    DK_SAMPLE_BANK_UNLOCK(R2);
    R0 = 0;
    JUMP (M13, I13);
_lp_reader_b_note_fault:
    DK_SAMPLE_BANK_FAIL_CLOSED(_lp_close_b_fault, _lp_close_b_closed, _lp_close_b_open, _lp_close_b_ready, R0);
    DK_SAMPLE_BANK_POISON_COUNT(_lp_close_b_count, R0);
_lp_reader_b_note_denied:
    DK_SAMPLE_BANK_UNLOCK(R2);
    JUMP _lp_reader_note_denied;
_lp_reader_b_render:
    R0 = I3; DM(_lp_reader_b_state) = R0;
    R0 = DM(_lp_reader_b_blocks); R0 = R0 + 1; DM(_lp_reader_b_blocks) = R0;
    DK_SAMPLE_BANK_LOCK(R2);
    // Cached ownership joins epoch/admission/count in the same masked scope.
    R0 = DM(8, I3); COMP(R0, R6); IF NE JUMP _lp_reader_b_render_denied;
    DK_SAMPLE_BANK_CHECK_ADMISSION(_lp_close_b_open, _lp_close_b_closed, _lp_close_b_count, R0, R1, _lp_reader_b_render_denied, _lp_reader_b_render_fault);
    R0 = DM(_lp_close_b_ready); R1 = 1; COMP(R0, R1); IF NE JUMP _lp_reader_b_render_denied;
    DK_SAMPLE_BANK_CHECK_EPOCH(_lp_close_b_epoch, I3, 6, R0, R1, _lp_reader_b_render_denied);
    DK_SAMPLE_BANK_ACQUIRE(_lp_close_b_count, R0);
    DK_SAMPLE_BANK_UNLOCK(R2);
.GLOBAL _lp_reader_b_render_acquired;
_lp_reader_b_render_acquired:
    R0 = DM(_lp_reader_b_reads); R0 = R0 + 1; DM(_lp_reader_b_reads) = R0;
    R0 = DM(DK_IN_W_OUT, I2); DM(_lp_reader_b_output) = R0;
    R3 = I12; R5 = I13;
    MODIFY(I7, -1) (NW);
    R4 = DM(7, I3);
    R8 = DM(DK_IN_W_OUT, I2);
    R12 = I3;
    DK_CALL(_dk_sample_pcm16_linear, _lp_reader_b_after_reader);
    R0 = DM(DK_IN_W_OUT, I2); I4 = R0;
    // Private callback-boundary observation, before stock stages mutate output.
    R0 = DM(_lp_reader_b_snapshot_seq); R0 = R0 + 1;
    DM(_lp_reader_b_snapshot_seq) = R0;
    I0 = _lp_reader_b_snapshot;
    R2 = 32;
    NOP; NOP; NOP;
_lp_reader_b_snapshot_read_copy:
    R1 = DM(I4, M6); DM(I0, M6) = R1;
    R2 = R2 - 1; R2 = PASS R2; IF GT JUMP _lp_reader_b_snapshot_read_copy;
    R0 = DM(_lp_reader_b_snapshot_seq); R0 = R0 + 1;
    DM(_lp_reader_b_snapshot_seq) = R0;
    MODIFY(I7, 1) (NW);
    I12 = R3; I13 = R5;
    DK_SAMPLE_BANK_LOCK(R2);
    DK_SAMPLE_BANK_RELEASE(_lp_close_b_count, R0, _lp_reader_b_render_release_fault, _lp_reader_b_render_released);
_lp_reader_b_render_release_fault:
    DK_SAMPLE_BANK_FAIL_CLOSED(_lp_close_b_fault, _lp_close_b_closed, _lp_close_b_open, _lp_close_b_ready, R0);
    DK_SAMPLE_BANK_POISON_COUNT(_lp_close_b_count, R0);
_lp_reader_b_render_released:
    DK_SAMPLE_BANK_UNLOCK(R2);
    R0 = DM(5, I3);
    JUMP (M13, I13);
_lp_reader_b_render_fault:
    DK_SAMPLE_BANK_FAIL_CLOSED(_lp_close_b_fault, _lp_close_b_closed, _lp_close_b_open, _lp_close_b_ready, R0);
    DK_SAMPLE_BANK_POISON_COUNT(_lp_close_b_count, R0);
_lp_reader_b_render_denied:
    DK_SAMPLE_BANK_UNLOCK(R2);
    JUMP _lp_reader_silent;

.SECTION/DM seg_dmda;
.GLOBAL _lp_play_desc;
.VAR _lp_play_desc = DK_MAGIC;
.VAR _lp_play_desc1 = DK_MACHINE_V1;
.VAR _lp_play_desc2 = 1;
.VAR _lp_play_desc3 = DK_MAX_VOICES;
.VAR _lp_play_desc4 = 36;
.VAR _lp_play_desc5 = 36;
// SDK ceiling 2662 (measured 1854 without the loop option) plus bounded wrapper and 32-word diagnostic copy.
// Instruction ceiling only; these historical fields do not measure hardware time.
.VAR _lp_play_desc6 = 2500;
.VAR _lp_play_desc7 = 300;
.VAR _lp_play_desc8 = 2;
.VAR _lp_play_desc9 = 128;
.GLOBAL _lp_reader_a_notes; .VAR _lp_reader_a_notes = 0;
.GLOBAL _lp_reader_a_blocks; .VAR _lp_reader_a_blocks = 0;
.GLOBAL _lp_reader_a_reads; .VAR _lp_reader_a_reads = 0;
.GLOBAL _lp_reader_a_state; .VAR _lp_reader_a_state = 0;
.GLOBAL _lp_reader_a_output; .VAR _lp_reader_a_output = 0;
.GLOBAL _lp_reader_b_notes; .VAR _lp_reader_b_notes = 0;
.GLOBAL _lp_reader_b_blocks; .VAR _lp_reader_b_blocks = 0;
.GLOBAL _lp_reader_b_reads; .VAR _lp_reader_b_reads = 0;
.GLOBAL _lp_reader_b_state; .VAR _lp_reader_b_state = 0;
.GLOBAL _lp_reader_b_output; .VAR _lp_reader_b_output = 0;

.GLOBAL _lp_reader_a_snapshot_seq; .VAR _lp_reader_a_snapshot_seq = 0;
.GLOBAL _lp_reader_a_snapshot;
.VAR _lp_reader_a_snapshot = 0;
.VAR _lp_reader_a_snapshot1 = 0;
.VAR _lp_reader_a_snapshot2 = 0;
.VAR _lp_reader_a_snapshot3 = 0;
.VAR _lp_reader_a_snapshot4 = 0;
.VAR _lp_reader_a_snapshot5 = 0;
.VAR _lp_reader_a_snapshot6 = 0;
.VAR _lp_reader_a_snapshot7 = 0;
.VAR _lp_reader_a_snapshot8 = 0;
.VAR _lp_reader_a_snapshot9 = 0;
.VAR _lp_reader_a_snapshot10 = 0;
.VAR _lp_reader_a_snapshot11 = 0;
.VAR _lp_reader_a_snapshot12 = 0;
.VAR _lp_reader_a_snapshot13 = 0;
.VAR _lp_reader_a_snapshot14 = 0;
.VAR _lp_reader_a_snapshot15 = 0;
.VAR _lp_reader_a_snapshot16 = 0;
.VAR _lp_reader_a_snapshot17 = 0;
.VAR _lp_reader_a_snapshot18 = 0;
.VAR _lp_reader_a_snapshot19 = 0;
.VAR _lp_reader_a_snapshot20 = 0;
.VAR _lp_reader_a_snapshot21 = 0;
.VAR _lp_reader_a_snapshot22 = 0;
.VAR _lp_reader_a_snapshot23 = 0;
.VAR _lp_reader_a_snapshot24 = 0;
.VAR _lp_reader_a_snapshot25 = 0;
.VAR _lp_reader_a_snapshot26 = 0;
.VAR _lp_reader_a_snapshot27 = 0;
.VAR _lp_reader_a_snapshot28 = 0;
.VAR _lp_reader_a_snapshot29 = 0;
.VAR _lp_reader_a_snapshot30 = 0;
.VAR _lp_reader_a_snapshot31 = 0;

.GLOBAL _lp_reader_b_snapshot_seq; .VAR _lp_reader_b_snapshot_seq = 0;
.GLOBAL _lp_reader_b_snapshot;
.VAR _lp_reader_b_snapshot = 0;
.VAR _lp_reader_b_snapshot1 = 0;
.VAR _lp_reader_b_snapshot2 = 0;
.VAR _lp_reader_b_snapshot3 = 0;
.VAR _lp_reader_b_snapshot4 = 0;
.VAR _lp_reader_b_snapshot5 = 0;
.VAR _lp_reader_b_snapshot6 = 0;
.VAR _lp_reader_b_snapshot7 = 0;
.VAR _lp_reader_b_snapshot8 = 0;
.VAR _lp_reader_b_snapshot9 = 0;
.VAR _lp_reader_b_snapshot10 = 0;
.VAR _lp_reader_b_snapshot11 = 0;
.VAR _lp_reader_b_snapshot12 = 0;
.VAR _lp_reader_b_snapshot13 = 0;
.VAR _lp_reader_b_snapshot14 = 0;
.VAR _lp_reader_b_snapshot15 = 0;
.VAR _lp_reader_b_snapshot16 = 0;
.VAR _lp_reader_b_snapshot17 = 0;
.VAR _lp_reader_b_snapshot18 = 0;
.VAR _lp_reader_b_snapshot19 = 0;
.VAR _lp_reader_b_snapshot20 = 0;
.VAR _lp_reader_b_snapshot21 = 0;
.VAR _lp_reader_b_snapshot22 = 0;
.VAR _lp_reader_b_snapshot23 = 0;
.VAR _lp_reader_b_snapshot24 = 0;
.VAR _lp_reader_b_snapshot25 = 0;
.VAR _lp_reader_b_snapshot26 = 0;
.VAR _lp_reader_b_snapshot27 = 0;
.VAR _lp_reader_b_snapshot28 = 0;
.VAR _lp_reader_b_snapshot29 = 0;
.VAR _lp_reader_b_snapshot30 = 0;
.VAR _lp_reader_b_snapshot31 = 0;
