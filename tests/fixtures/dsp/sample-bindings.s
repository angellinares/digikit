// Private two-track consumer/producer proof; no public resource ABI.
#include "dk_machine.inc"
#include "dsp/call.inc"
#include "dsp/sample.inc"
#include "dsp/sample_pcm16.inc"
#include "dsp/sample_bank.inc"
.SECTION/DM seg_dmda;
.GLOBAL _bind_a_open;
.VAR _bind_a_open = 0;
.GLOBAL _bind_a_closed;
.VAR _bind_a_closed = 1;
.GLOBAL _bind_a_count;
.VAR _bind_a_count = 0;
.GLOBAL _bind_a_fault;
.VAR _bind_a_fault = 0;
.GLOBAL _bind_a_ready;
.VAR _bind_a_ready = 0;
.GLOBAL _bind_a_token;
.VAR _bind_a_token = 0;
.GLOBAL _bind_a_source;
.VAR _bind_a_source = 0;
.GLOBAL _bind_a_frames;
.VAR _bind_a_frames = 0;
.GLOBAL _bind_a_payload;
.VAR _bind_a_payload = 268443648;
.PKDATA _bind_a_pcm, 256;
.GLOBAL _bind_b_open;
.VAR _bind_b_open = 0;
.GLOBAL _bind_b_closed;
.VAR _bind_b_closed = 1;
.GLOBAL _bind_b_count;
.VAR _bind_b_count = 0;
.GLOBAL _bind_b_fault;
.VAR _bind_b_fault = 0;
.GLOBAL _bind_b_ready;
.VAR _bind_b_ready = 0;
.GLOBAL _bind_b_token;
.VAR _bind_b_token = 0;
.GLOBAL _bind_b_source;
.VAR _bind_b_source = 0;
.GLOBAL _bind_b_frames;
.VAR _bind_b_frames = 0;
.GLOBAL _bind_b_payload;
.VAR _bind_b_payload = 3758112768;
.PKDATA _bind_b_pcm, 256;
.SECTION/PM seg_pmco;
.GLOBAL _bind_note_on;
.GLOBAL _bind_render;
.GLOBAL _bind_close;
.GLOBAL _bind_publish;
// Voice: six SDK PCM cursor words, epoch+6, source+7, cached track+8.
_bind_note_on:
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
    R0 = 0; COMP(R6, R0); IF EQ JUMP _bind_a_note;
    R0 = 1; COMP(R6, R0); IF EQ JUMP _bind_b_note;
    JUMP _bind_note_denied;
_bind_render:
    I3 = R4;
    I2 = R8;
    NOP; NOP; NOP;
    R6 = DM(DK_IN_W_TRACK, I2);
    R0 = 0; COMP(R6, R0); IF EQ JUMP _bind_a_render;
    R0 = 1; COMP(R6, R0); IF EQ JUMP _bind_b_render;
    JUMP _bind_silent;
_bind_close:
    R0 = 0; COMP(R4, R0); IF EQ JUMP _bind_a_close;
    R0 = 1; COMP(R4, R0); IF EQ JUMP _bind_b_close;
    JUMP _bind_invalid;
_bind_publish:
    R0 = 0; COMP(R4, R0); IF EQ JUMP _bind_a_publish;
    R0 = 1; COMP(R4, R0); IF EQ JUMP _bind_b_publish;
_bind_invalid:
    R0 = -1;
    JUMP (M13, I13);
_bind_note_denied:
    R0 = 1;
    JUMP (M13, I13);
_bind_silent:
    R1 = DM(DK_IN_W_OUT, I2);
    I2 = R1;
    R0 = 0;
    R2 = 32;
    NOP; NOP; NOP;
_bind_zero:
    DM(I2, M6) = R0;
    R2 = R2 - 1;
    R2 = PASS R2;
    IF GT JUMP _bind_zero;
    R0 = 1; DM(5, I3) = R0;
    JUMP (M13, I13);
_bind_a_note:
    DK_SAMPLE_BANK_LOCK(R2);
    DK_SAMPLE_BANK_CHECK_ADMISSION(_bind_a_open, _bind_a_closed, _bind_a_count, R0, R1, _bind_a_note_denied, _bind_a_note_fault);
    DK_SAMPLE_BANK_LATCH_EPOCH(_bind_a_ready, _bind_a_token, I3, 6, R1, R0, _bind_a_note_denied);
    DK_SAMPLE_BANK_ACQUIRE(_bind_a_count, R0);
    DK_SAMPLE_BANK_UNLOCK(R2);
.GLOBAL _bind_a_note_acquired;
_bind_a_note_acquired:
    R0 = DM(_bind_a_source); DM(7, I3) = R0;
    R0 = DM(_bind_a_frames); DM(1, I3) = R0;
    R0 = 0; DM(5, I3) = R0;
    DK_SAMPLE_BANK_LOCK(R2);
    DK_SAMPLE_BANK_RELEASE(_bind_a_count, R0, _bind_a_note_release_fault, _bind_a_note_released);
_bind_a_note_release_fault:
    DK_SAMPLE_BANK_FAIL_CLOSED(_bind_a_fault, _bind_a_closed, _bind_a_open, _bind_a_ready, R0);
    DK_SAMPLE_BANK_POISON_COUNT(_bind_a_count, R0);
_bind_a_note_released:
    DK_SAMPLE_BANK_UNLOCK(R2);
    R0 = 0;
    JUMP (M13, I13);
_bind_a_note_fault:
    DK_SAMPLE_BANK_FAIL_CLOSED(_bind_a_fault, _bind_a_closed, _bind_a_open, _bind_a_ready, R0);
    DK_SAMPLE_BANK_POISON_COUNT(_bind_a_count, R0);
_bind_a_note_denied:
    DK_SAMPLE_BANK_UNLOCK(R2);
    JUMP _bind_note_denied;
_bind_a_render:
    DK_SAMPLE_BANK_LOCK(R2);
    // Cached ownership joins epoch/admission/count in the same masked scope.
    R0 = DM(8, I3); COMP(R0, R6); IF NE JUMP _bind_a_render_denied;
    DK_SAMPLE_BANK_CHECK_ADMISSION(_bind_a_open, _bind_a_closed, _bind_a_count, R0, R1, _bind_a_render_denied, _bind_a_render_fault);
    DK_SAMPLE_BANK_CHECK_EPOCH(_bind_a_token, I3, 6, R0, R1, _bind_a_render_denied);
    DK_SAMPLE_BANK_ACQUIRE(_bind_a_count, R0);
    DK_SAMPLE_BANK_UNLOCK(R2);
.GLOBAL _bind_a_render_acquired;
_bind_a_render_acquired:
    R3 = I12; R5 = I13;
    MODIFY(I7, -1) (NW);
    R4 = DM(7, I3);
    R8 = DM(DK_IN_W_OUT, I2);
    R12 = I3;
    DK_CALL(_dk_sample_pcm16_linear, _bind_a_after_reader);
    MODIFY(I7, 1) (NW);
    I12 = R3; I13 = R5;
    DK_SAMPLE_BANK_LOCK(R2);
    DK_SAMPLE_BANK_RELEASE(_bind_a_count, R0, _bind_a_render_release_fault, _bind_a_render_released);
_bind_a_render_release_fault:
    DK_SAMPLE_BANK_FAIL_CLOSED(_bind_a_fault, _bind_a_closed, _bind_a_open, _bind_a_ready, R0);
    DK_SAMPLE_BANK_POISON_COUNT(_bind_a_count, R0);
_bind_a_render_released:
    DK_SAMPLE_BANK_UNLOCK(R2);
    R0 = DM(5, I3);
    JUMP (M13, I13);
_bind_a_render_fault:
    DK_SAMPLE_BANK_FAIL_CLOSED(_bind_a_fault, _bind_a_closed, _bind_a_open, _bind_a_ready, R0);
    DK_SAMPLE_BANK_POISON_COUNT(_bind_a_count, R0);
_bind_a_render_denied:
    DK_SAMPLE_BANK_UNLOCK(R2);
    JUMP _bind_silent;
_bind_a_close:
    DK_SAMPLE_BANK_LOCK(R7);
    R0 = DM(_bind_a_token); COMP(R0, R8); IF NE JUMP _bind_a_producer_invalid;
    DK_SAMPLE_BANK_CLOSE(_bind_a_open, _bind_a_ready, _bind_a_closed, R0);
    DK_SAMPLE_BANK_CHECK_DRAIN(_bind_a_count, _bind_a_fault, R0, R1, _bind_a_producer_pending);
    JUMP _bind_a_producer_ok;
_bind_a_publish:
    DK_SAMPLE_BANK_LOCK(R7);
    R0 = DM(_bind_a_token); COMP(R0, R8); IF NE JUMP _bind_a_producer_invalid;
    R0 = PASS R0; IF LT JUMP _bind_a_producer_invalid;
    R1 = PASS R12; IF LE JUMP _bind_a_producer_invalid;
    COMP(R12, R0); IF LE JUMP _bind_a_producer_invalid;
    R0 = DM(_bind_a_closed); R1 = 1; COMP(R0, R1); IF NE JUMP _bind_a_producer_invalid;
    R0 = DM(_bind_a_open); R0 = PASS R0; IF NE JUMP _bind_a_producer_invalid;
    R0 = DM(_bind_a_ready); R0 = PASS R0; IF NE JUMP _bind_a_producer_invalid;
    DK_SAMPLE_BANK_CHECK_DRAIN(_bind_a_count, _bind_a_fault, R0, R1, _bind_a_producer_pending);
    DK_SAMPLE_BANK_UNLOCK(R7);
    // Private bounded producer models completed payload delivery, not CF/DMA.
    // One serialized producer; admission stays closed throughout this fill.
    R2 = _bind_a_pcm; R2 = LSHIFT R2 BY -2; I0 = R2;
    R3 = DM(_bind_a_payload); R6 = 64;
    NOP; NOP; NOP;
_bind_a_fill:
    DM(I0, M6) = R3;
    R6 = R6 - 1; R6 = PASS R6; IF GT JUMP _bind_a_fill;
    DK_SAMPLE_BANK_LOCK(R7);
    R0 = DM(_bind_a_token); COMP(R0, R8); IF NE JUMP _bind_a_producer_invalid;
    R0 = PASS R0; IF LT JUMP _bind_a_producer_invalid;
    R1 = PASS R12; IF LE JUMP _bind_a_producer_invalid;
    COMP(R12, R0); IF LE JUMP _bind_a_producer_invalid;
    R0 = DM(_bind_a_closed); R1 = 1; COMP(R0, R1); IF NE JUMP _bind_a_producer_invalid;
    R0 = DM(_bind_a_open); R0 = PASS R0; IF NE JUMP _bind_a_producer_invalid;
    R0 = DM(_bind_a_ready); R0 = PASS R0; IF NE JUMP _bind_a_producer_invalid;
    DK_SAMPLE_BANK_CHECK_DRAIN(_bind_a_count, _bind_a_fault, R0, R1, _bind_a_producer_pending);
    // Descriptor and token become immutable before READY is published last.
    DM(_bind_a_source) = R2; R0 = 128; DM(_bind_a_frames) = R0;
    DM(_bind_a_token) = R12;
    R0 = 0; DM(_bind_a_closed) = R0;
.GLOBAL _bind_a_before_ready;
_bind_a_before_ready:
    DK_SAMPLE_BANK_PUBLISH(_bind_a_open, _bind_a_ready, R0);
_bind_a_producer_ok:
    DK_SAMPLE_BANK_UNLOCK(R7); R0 = 0; JUMP (M13, I13);
_bind_a_producer_pending:
    DK_SAMPLE_BANK_UNLOCK(R7); R0 = 2; JUMP (M13, I13);
_bind_a_producer_invalid:
    DK_SAMPLE_BANK_UNLOCK(R7); R0 = -1; JUMP (M13, I13);
_bind_b_note:
    DK_SAMPLE_BANK_LOCK(R2);
    DK_SAMPLE_BANK_CHECK_ADMISSION(_bind_b_open, _bind_b_closed, _bind_b_count, R0, R1, _bind_b_note_denied, _bind_b_note_fault);
    DK_SAMPLE_BANK_LATCH_EPOCH(_bind_b_ready, _bind_b_token, I3, 6, R1, R0, _bind_b_note_denied);
    DK_SAMPLE_BANK_ACQUIRE(_bind_b_count, R0);
    DK_SAMPLE_BANK_UNLOCK(R2);
.GLOBAL _bind_b_note_acquired;
_bind_b_note_acquired:
    R0 = DM(_bind_b_source); DM(7, I3) = R0;
    R0 = DM(_bind_b_frames); DM(1, I3) = R0;
    R0 = 0; DM(5, I3) = R0;
    DK_SAMPLE_BANK_LOCK(R2);
    DK_SAMPLE_BANK_RELEASE(_bind_b_count, R0, _bind_b_note_release_fault, _bind_b_note_released);
_bind_b_note_release_fault:
    DK_SAMPLE_BANK_FAIL_CLOSED(_bind_b_fault, _bind_b_closed, _bind_b_open, _bind_b_ready, R0);
    DK_SAMPLE_BANK_POISON_COUNT(_bind_b_count, R0);
_bind_b_note_released:
    DK_SAMPLE_BANK_UNLOCK(R2);
    R0 = 0;
    JUMP (M13, I13);
_bind_b_note_fault:
    DK_SAMPLE_BANK_FAIL_CLOSED(_bind_b_fault, _bind_b_closed, _bind_b_open, _bind_b_ready, R0);
    DK_SAMPLE_BANK_POISON_COUNT(_bind_b_count, R0);
_bind_b_note_denied:
    DK_SAMPLE_BANK_UNLOCK(R2);
    JUMP _bind_note_denied;
_bind_b_render:
    DK_SAMPLE_BANK_LOCK(R2);
    // Cached ownership joins epoch/admission/count in the same masked scope.
    R0 = DM(8, I3); COMP(R0, R6); IF NE JUMP _bind_b_render_denied;
    DK_SAMPLE_BANK_CHECK_ADMISSION(_bind_b_open, _bind_b_closed, _bind_b_count, R0, R1, _bind_b_render_denied, _bind_b_render_fault);
    DK_SAMPLE_BANK_CHECK_EPOCH(_bind_b_token, I3, 6, R0, R1, _bind_b_render_denied);
    DK_SAMPLE_BANK_ACQUIRE(_bind_b_count, R0);
    DK_SAMPLE_BANK_UNLOCK(R2);
.GLOBAL _bind_b_render_acquired;
_bind_b_render_acquired:
    R3 = I12; R5 = I13;
    MODIFY(I7, -1) (NW);
    R4 = DM(7, I3);
    R8 = DM(DK_IN_W_OUT, I2);
    R12 = I3;
    DK_CALL(_dk_sample_pcm16_linear, _bind_b_after_reader);
    MODIFY(I7, 1) (NW);
    I12 = R3; I13 = R5;
    DK_SAMPLE_BANK_LOCK(R2);
    DK_SAMPLE_BANK_RELEASE(_bind_b_count, R0, _bind_b_render_release_fault, _bind_b_render_released);
_bind_b_render_release_fault:
    DK_SAMPLE_BANK_FAIL_CLOSED(_bind_b_fault, _bind_b_closed, _bind_b_open, _bind_b_ready, R0);
    DK_SAMPLE_BANK_POISON_COUNT(_bind_b_count, R0);
_bind_b_render_released:
    DK_SAMPLE_BANK_UNLOCK(R2);
    R0 = DM(5, I3);
    JUMP (M13, I13);
_bind_b_render_fault:
    DK_SAMPLE_BANK_FAIL_CLOSED(_bind_b_fault, _bind_b_closed, _bind_b_open, _bind_b_ready, R0);
    DK_SAMPLE_BANK_POISON_COUNT(_bind_b_count, R0);
_bind_b_render_denied:
    DK_SAMPLE_BANK_UNLOCK(R2);
    JUMP _bind_silent;
_bind_b_close:
    DK_SAMPLE_BANK_LOCK(R7);
    R0 = DM(_bind_b_token); COMP(R0, R8); IF NE JUMP _bind_b_producer_invalid;
    DK_SAMPLE_BANK_CLOSE(_bind_b_open, _bind_b_ready, _bind_b_closed, R0);
    DK_SAMPLE_BANK_CHECK_DRAIN(_bind_b_count, _bind_b_fault, R0, R1, _bind_b_producer_pending);
    JUMP _bind_b_producer_ok;
_bind_b_publish:
    DK_SAMPLE_BANK_LOCK(R7);
    R0 = DM(_bind_b_token); COMP(R0, R8); IF NE JUMP _bind_b_producer_invalid;
    R0 = PASS R0; IF LT JUMP _bind_b_producer_invalid;
    R1 = PASS R12; IF LE JUMP _bind_b_producer_invalid;
    COMP(R12, R0); IF LE JUMP _bind_b_producer_invalid;
    R0 = DM(_bind_b_closed); R1 = 1; COMP(R0, R1); IF NE JUMP _bind_b_producer_invalid;
    R0 = DM(_bind_b_open); R0 = PASS R0; IF NE JUMP _bind_b_producer_invalid;
    R0 = DM(_bind_b_ready); R0 = PASS R0; IF NE JUMP _bind_b_producer_invalid;
    DK_SAMPLE_BANK_CHECK_DRAIN(_bind_b_count, _bind_b_fault, R0, R1, _bind_b_producer_pending);
    DK_SAMPLE_BANK_UNLOCK(R7);
    // Private bounded producer models completed payload delivery, not CF/DMA.
    // One serialized producer; admission stays closed throughout this fill.
    R2 = _bind_b_pcm; R2 = LSHIFT R2 BY -2; I0 = R2;
    R3 = DM(_bind_b_payload); R6 = 64;
    NOP; NOP; NOP;
_bind_b_fill:
    DM(I0, M6) = R3;
    R6 = R6 - 1; R6 = PASS R6; IF GT JUMP _bind_b_fill;
    DK_SAMPLE_BANK_LOCK(R7);
    R0 = DM(_bind_b_token); COMP(R0, R8); IF NE JUMP _bind_b_producer_invalid;
    R0 = PASS R0; IF LT JUMP _bind_b_producer_invalid;
    R1 = PASS R12; IF LE JUMP _bind_b_producer_invalid;
    COMP(R12, R0); IF LE JUMP _bind_b_producer_invalid;
    R0 = DM(_bind_b_closed); R1 = 1; COMP(R0, R1); IF NE JUMP _bind_b_producer_invalid;
    R0 = DM(_bind_b_open); R0 = PASS R0; IF NE JUMP _bind_b_producer_invalid;
    R0 = DM(_bind_b_ready); R0 = PASS R0; IF NE JUMP _bind_b_producer_invalid;
    DK_SAMPLE_BANK_CHECK_DRAIN(_bind_b_count, _bind_b_fault, R0, R1, _bind_b_producer_pending);
    // Descriptor and token become immutable before READY is published last.
    DM(_bind_b_source) = R2; R0 = 128; DM(_bind_b_frames) = R0;
    DM(_bind_b_token) = R12;
    R0 = 0; DM(_bind_b_closed) = R0;
.GLOBAL _bind_b_before_ready;
_bind_b_before_ready:
    DK_SAMPLE_BANK_PUBLISH(_bind_b_open, _bind_b_ready, R0);
_bind_b_producer_ok:
    DK_SAMPLE_BANK_UNLOCK(R7); R0 = 0; JUMP (M13, I13);
_bind_b_producer_pending:
    DK_SAMPLE_BANK_UNLOCK(R7); R0 = 2; JUMP (M13, I13);
_bind_b_producer_invalid:
    DK_SAMPLE_BANK_UNLOCK(R7); R0 = -1; JUMP (M13, I13);
