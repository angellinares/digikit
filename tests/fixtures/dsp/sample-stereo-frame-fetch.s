// Private exclusion experiment only. Original guard helper is supplied from a
// hash-bound runtime DT2 1.17 image; no original instruction bytes are embedded.
#include "dsp/call.inc"
#include "dsp/sample.inc"
#include "dsp/sample_pcm16.inc"
#include "dsp/sample_pcm16_fetch.inc"
#include "dsp/sample_pcm16_stereo.inc"
#include "dsp/sample_bank.inc"
.SECTION/PM seg_pmco;
.EXTERN _ff_original_guard;
.GLOBAL _ff_guard_h;
_ff_guard_h:
    DK_CALL(_ff_original_guard, _ff_guard_return);
    RTS;

// Cursor, outputs, save frame and packed-tap scratch remain callback-owned even
// after count reaches zero. Writer may close admission/reuse BACKING ONLY.
.SECTION/DM seg_dmda;
.GLOBAL _ff_open; .VAR _ff_open = 1;
.GLOBAL _ff_closed; .VAR _ff_closed = 0;
.GLOBAL _ff_count; .VAR _ff_count = 0;
.GLOBAL _ff_fault; .VAR _ff_fault = 0;
.GLOBAL _ff_ready; .VAR _ff_ready = 1;
.GLOBAL _ff_token; .VAR _ff_token = 1;
.GLOBAL _ff_sources; .VAR _ff_sources = 0; .VAR _ff_source_right = 0;
.GLOBAL _ff_spans; .VAR _ff_spans = 0; .VAR _ff_span_left_end = 0;
.VAR _ff_span_right = 0; .VAR _ff_span_right_end = 0;
.GLOBAL _ff_cursor;
.VAR _ff_cursor = 0; .VAR _ff_end = 65; .VAR _ff_index = 0;
.VAR _ff_frac = 32768; .VAR _ff_rate = 65536; .VAR _ff_done = 0;
.VAR _ff_epoch = 1;
.GLOBAL _ff_fetched; .VAR _ff_fetched = 0;
.GLOBAL _ff_split_control; .VAR _ff_split_control = 0;
.GLOBAL _ff_written; .VAR _ff_written = 0;
.GLOBAL _ff_write_count; .VAR _ff_write_count = 0;
.GLOBAL _ff_write_fault; .VAR _ff_write_fault = 0;

.SECTION/PM seg_pmco;
.GLOBAL _ff_reader_h;
_ff_reader_h:
    DK_CALL(_ff_reader, _ff_reader_return);
    RTS;
_ff_reader:
    DK_ENTER(24);
    DK_SAVE(2, R3); DK_SAVE(3, R5); DK_SAVE(4, R6); DK_SAVE(5, R7);
    DK_SAVE(6, R9); DK_SAVE(7, R10); DK_SAVE(8, R11); DK_SAVE(9, R13);
    DK_SAVE(10, R14); DK_SAVE(11, R15);
    R0 = I0; DK_SAVE(12, R0); R0 = I1; DK_SAVE(13, R0);
    R0 = I2; DK_SAVE(14, R0); R0 = I3; DK_SAVE(15, R0);
    I0 = 0x08036000; I2 = 0x08036020;
    R0 = _ff_cursor; R0 = LSHIFT R0 BY -2; I1 = R0;
    R0 = _ff_sources; R0 = LSHIFT R0 BY -2; I3 = R0;
    R13 = 65535; R15 = 32;
_ff_frame:
    DK_SAMPLE_BANK_LOCK(R3);
    DK_SAMPLE_BANK_CHECK_ADMISSION(_ff_open, _ff_closed, _ff_count, R0, R1, _ff_denied, _ff_faulted);
    DK_SAMPLE_BANK_CHECK_EPOCH(_ff_token, I1, 6, R0, R1, _ff_denied);
    DK_SAMPLE_BANK_ACQUIRE(_ff_count, R0);
    R5 = DM(0, I1); R6 = DM(1, I1); R7 = DM(2, I1);
    R9 = DM(3, I1); R0 = DM(4, I1); DM(-21, I6) = R0;
    R14 = DM(5, I1);
    COMP(R5, R6); IF GE JUMP _ff_finished;
    COMP(R7, R5); IF LT JUMP _ff_finished;
    COMP(R7, R6); IF GE JUMP _ff_finished;
    R0 = PASS R14; IF NE JUMP _ff_finished;
    R12 = R7 + 1; COMP(R12, R6); IF LT JUMP _ff_neighbor;
    R12 = R7;
_ff_neighbor:
    DM(-20, I6) = R12;
    R4 = DM(0, I3);
    DK_SAMPLE_PCM16_FETCH_WORDS(R4, R7, R12, R8, R0, R1, I4);
    DM(-16, I6) = R8; DM(-17, I6) = R0;
    // Deliberately unsafe control: deliver between channel fetches after count0.
    // Only this control skips re-admission for right; the real path stays locked.
    R0 = DM(_ff_split_control); R0 = PASS R0;
    IF EQ JUMP _ff_right_fetch;
    DK_SAMPLE_BANK_RELEASE(_ff_count, R0, _ff_faulted, _ff_split_released);
_ff_split_released:
    DK_SAMPLE_BANK_UNLOCK(R3);
.GLOBAL _ff_split_boundary;
_ff_split_boundary:
    NOP;
    DK_SAMPLE_BANK_LOCK(R3);
_ff_right_fetch:
    R4 = DM(1, I3);
    DK_SAMPLE_PCM16_FETCH_WORDS(R4, R7, R12, R8, R0, R1, I4);
    DM(-18, I6) = R8; DM(-19, I6) = R0;
    R0 = DM(_ff_split_control); R0 = PASS R0; IF NE JUMP _ff_released;
    DK_SAMPLE_BANK_RELEASE(_ff_count, R0, _ff_faulted, _ff_released);
_ff_released:
    R0 = DM(_ff_fetched); R0 = R0 + 1; DM(_ff_fetched) = R0;
    DK_SAMPLE_BANK_UNLOCK(R3);
    // All subsequent reads until next _ff_frame are CALLBACK-PRIVATE copies.
    R0 = DM(-21, I6); R2 = -16; R10 = ASHIFT R0 BY R2;
    R11 = R0 AND R13;
    R2 = 1; R1 = R7 AND R2; R1 = LSHIFT R1 BY 4;
    R2 = 16; R1 = R2 - R1;
    R0 = DM(-16, I6); R0 = LSHIFT R0 BY R1;
    R2 = -16; R0 = ASHIFT R0 BY R2; R2 = -15; F0 = FLOAT R0 BY R2;
    R12 = DM(-20, I6); R2 = 1; R1 = R12 AND R2;
    R1 = LSHIFT R1 BY 4; R2 = 16; R1 = R2 - R1;
    R4 = DM(-17, I6); R1 = LSHIFT R4 BY R1;
    R2 = -16; R1 = ASHIFT R1 BY R2; F4 = FLOAT R9 BY R2;
    R2 = -15; F1 = FLOAT R1 BY R2;
    F2 = F1 - F0; F1 = F4 * F2; F8 = F0 + F1;
    DM(I0, M6) = R8;
    R2 = 1; R1 = R7 AND R2; R1 = LSHIFT R1 BY 4;
    R2 = 16; R1 = R2 - R1;
    R0 = DM(-18, I6); R0 = LSHIFT R0 BY R1;
    R2 = -16; R0 = ASHIFT R0 BY R2; R2 = -15; F0 = FLOAT R0 BY R2;
    R12 = DM(-20, I6); R2 = 1; R1 = R12 AND R2;
    R1 = LSHIFT R1 BY 4; R2 = 16; R1 = R2 - R1;
    R4 = DM(-19, I6); R1 = LSHIFT R4 BY R1;
    R2 = -16; R1 = ASHIFT R1 BY R2; F4 = FLOAT R9 BY R2;
    R2 = -15; F1 = FLOAT R1 BY R2;
    F2 = F1 - F0; F1 = F4 * F2; F8 = F0 + F1;
    DM(I2, M6) = R8;
    DK_SAMPLE_CURSOR_STEP(R7, R9, R10, R11, R13, R0, R1);
    COMP(R7, R5); IF LT JUMP _ff_crossed;
    COMP(R7, R6); IF GE JUMP _ff_crossed;
    JUMP _ff_cursor_store;
_ff_crossed:
    R14 = 1;
_ff_cursor_store:
    DM(2, I1) = R7; DM(3, I1) = R9; DM(5, I1) = R14;
    R15 = R15 - 1; R15 = PASS R15; IF GT JUMP _ff_frame;
    R0 = 0; JUMP _ff_return;
_ff_finished:
    R14 = 1; DM(5, I1) = R14;
    DK_SAMPLE_BANK_RELEASE(_ff_count, R0, _ff_faulted, _ff_finish_released);
_ff_finish_released:
    DK_SAMPLE_BANK_UNLOCK(R3);
    JUMP _ff_zero_remaining;
_ff_faulted:
    DK_SAMPLE_BANK_FAIL_CLOSED(_ff_fault, _ff_closed, _ff_open, _ff_ready, R0);
    DK_SAMPLE_BANK_POISON_COUNT(_ff_count, R0);
_ff_denied:
    DK_SAMPLE_BANK_UNLOCK(R3);
_ff_zero_remaining:
    R0 = 0;
_ff_zero:
    DM(I0, M6) = R0; DM(I2, M6) = R0;
    R15 = R15 - 1; R15 = PASS R15; IF GT JUMP _ff_zero;
    R0 = 1;
_ff_return:
    DK_RETADDR;
    DK_RESTORE(R3, 2); DK_RESTORE(R5, 3); DK_RESTORE(R6, 4);
    DK_RESTORE(R7, 5); DK_RESTORE(R9, 6); DK_RESTORE(R10, 7);
    DK_RESTORE(R11, 8); DK_RESTORE(R13, 9); DK_RESTORE(R14, 10);
    DK_RESTORE(R15, 11); DK_RESTORE(I0, 12); DK_RESTORE(I1, 13);
    DK_RESTORE(I2, 14); DK_LEAVE(I3, 15);

// All admitted DDR byte extents lie in the same signed-negative interval;
// ordered signed comparisons plus end>start reject wrap before any store.
// Real private writer: physical affected start R4,length R8, guard-source offset
// R12 (ignored for page). Nonwrapping DDR extents validated before CLOSE.
// Any overlap with either full packed plane retires the WHOLE binding.
// R8=10 calls original guard, with destination R4 and source BASE+R12.
// R8=4096 fills exactly the requested physical page using 32-bit stores.
.GLOBAL _ff_writer_h;
_ff_writer_h:
    DK_CALL(_ff_writer, _ff_writer_return);
    RTS;
_ff_writer:
    DK_ENTER(4);
    DK_SAVE(2, R7); DK_SAVE(3, R5);
    DK_SAVE(4, R4); DK_SAVE(5, R8);
    DK_SAMPLE_BANK_LOCK(R7);
    R0 = 0; COMP(R8, R0); IF LE JUMP _ff_writer_invalid;
    R2 = R4 + R8;
    COMP(R2, R4); IF LE JUMP _ff_writer_invalid;
    R0 = 0x80000000; COMP(R4, R0); IF LT JUMP _ff_writer_invalid;
    R0 = 0xa0000000; COMP(R2, R0); IF GT JUMP _ff_writer_invalid;
    R0 = 4096; COMP(R8, R0); IF EQ JUMP _ff_writer_page_check;
    R0 = 10; COMP(R8, R0); IF NE JUMP _ff_writer_invalid;
    R0 = 1; R1 = R4 AND R0; R1 = PASS R1; IF NE JUMP _ff_writer_invalid;
    // Validate original helper source extent and nonoverlap, without remapping.
    R0 = 0x8422b9c8; R1 = R0 + R12; R5 = 10; R5 = R1 + R5;
    COMP(R1, R0); IF LT JUMP _ff_writer_invalid;
    COMP(R5, R1); IF LE JUMP _ff_writer_invalid;
    R0 = 0xa0000000; COMP(R5, R0); IF GT JUMP _ff_writer_invalid;
    R0 = 1; R0 = R1 AND R0; R0 = PASS R0; IF NE JUMP _ff_writer_invalid;
    COMP(R2, R1); IF LE JUMP _ff_writer_overlap;
    COMP(R4, R5); IF LT JUMP _ff_writer_invalid;
    JUMP _ff_writer_overlap;
_ff_writer_page_check:
    R0 = 0x8422b9c8; COMP(R4, R0); IF LT JUMP _ff_writer_invalid;
    R0 = R4 - R0; R1 = 4095; R1 = R0 AND R1;
    R1 = PASS R1; IF NE JUMP _ff_writer_invalid;
    R0 = LSHIFT R0 BY -12; R1 = 0x19200;
    COMP(R0, R1); IF GE JUMP _ff_writer_invalid;
_ff_writer_overlap:
    R0 = _ff_spans; R0 = LSHIFT R0 BY -2; I4 = R0; R5 = 2;
_ff_span:
    R0 = DM(I4, M6); R1 = DM(I4, M6);
    COMP(R2, R0); IF LE JUMP _ff_next_span;
    COMP(R4, R1); IF LT JUMP _ff_writer_hit;
_ff_next_span:
    R5 = R5 - 1; R5 = PASS R5; IF GT JUMP _ff_span;
    R0 = 0; JUMP _ff_writer_exit;
_ff_writer_hit:
    DK_SAMPLE_BANK_CLOSE(_ff_open, _ff_ready, _ff_closed, R0);
    DK_SAMPLE_BANK_CHECK_DRAIN(_ff_count, _ff_fault, R0, R1, _ff_writer_busy);
    R0 = DM(_ff_count); DM(_ff_write_count) = R0;
    R0 = DM(_ff_fault); DM(_ff_write_fault) = R0;
    R0 = 10; COMP(R8, R0); IF EQ JUMP _ff_writer_guard;
    I4 = R4; R0 = 0x30002000; R1 = 1024;
_ff_page:
    DM(I4, M6) = R0;
    R1 = R1 - 1; R1 = PASS R1; IF GT JUMP _ff_page;
    JUMP _ff_writer_written;
_ff_writer_guard:
    R0 = 0x8422b9c8; R8 = R4 - R0; R8 = R8 - R12;
    R4 = R12;
    DK_CALL(_ff_original_guard, _ff_guard_done);
_ff_writer_written:
    R0 = 1; DM(_ff_written) = R0; R0 = 0; JUMP _ff_writer_exit;
_ff_writer_busy:
    R0 = 2; JUMP _ff_writer_exit;
_ff_writer_invalid:
    R0 = 3;
 .GLOBAL _ff_writer_exit;
_ff_writer_exit:
    DK_SAMPLE_BANK_UNLOCK(R7);
    DK_RETADDR;
    DK_RESTORE(R7, 2);
    // Four intervening instructions after I12 load, per call.inc DAG latency.
    NOP; NOP; NOP;
 .GLOBAL _ff_writer_leave;
_ff_writer_leave:
    DK_LEAVE(R5, 3);

.SECTION/DM seg_dmda;
.GLOBAL _ff_outputs;
.VAR _ff_outputs = 0x08036000; .VAR _ff_output_right = 0x08036020;
.SECTION/PM seg_pmco;
.GLOBAL _ff_reference_h;
_ff_reference_h:
    R4 = _ff_sources; R4 = LSHIFT R4 BY -2;
    R8 = _ff_outputs; R8 = LSHIFT R8 BY -2;
    R12 = _ff_cursor; R12 = LSHIFT R12 BY -2;
    DK_CALL(_dk_sample_pcm16_stereo_linear, _ff_reference_return);
    RTS;
