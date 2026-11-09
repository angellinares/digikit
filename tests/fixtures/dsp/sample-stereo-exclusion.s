// Bounded exclusion experiment, NOT a production callback or masking policy.
// One represented binding's physical byte span crosses a page/guard boundary.
// Tests give writes of 4096 page bytes or 10 guard bytes; any overlap retires
// this whole represented binding. PCM planes are numerical stand-ins, not a
// claim that their actual bytes fit the small represented overlap span.
#include "dsp/call.inc"
#include "dsp/sample.inc"
#include "dsp/sample_pcm16.inc"
#include "dsp/sample_pcm16_stereo.inc"
#include "dsp/sample_bank.inc"
.SECTION/DM seg_dmda;
.GLOBAL _sx_open;
.VAR _sx_open = 1;
.GLOBAL _sx_closed;
.VAR _sx_closed = 0;
.GLOBAL _sx_count;
.VAR _sx_count = 0;
.GLOBAL _sx_fault;
.VAR _sx_fault = 0;
.GLOBAL _sx_ready;
.VAR _sx_ready = 1;
.GLOBAL _sx_token;
.VAR _sx_token = 1;
.GLOBAL _sx_span;
.VAR _sx_span = 16781308;
.VAR _sx_span_1 = 16781318;
.GLOBAL _sx_sources;
.VAR _sx_sources = 0;
.VAR _sx_sources_1 = 0;
.GLOBAL _sx_outputs;
.VAR _sx_outputs = 134438912;
.VAR _sx_outputs_1 = 134438944;
.GLOBAL _sx_cursor;
.VAR _sx_cursor = 0;
.VAR _sx_cursor_1 = 128;
.VAR _sx_cursor_2 = 0;
.VAR _sx_cursor_3 = 32768;
.VAR _sx_cursor_4 = 65536;
.VAR _sx_cursor_5 = 0;
.VAR _sx_cursor_6 = 1;
.GLOBAL _sx_unlocked_control;
.VAR _sx_unlocked_control = 0;
.GLOBAL _sx_written;
.VAR _sx_written = 0;
.GLOBAL _sx_write_count;
.VAR _sx_write_count = 0;
.SECTION/PM seg_pmco;
.GLOBAL _sx_reader_h;
_sx_reader_h:
    DK_CALL(_sx_reader, _sx_reader_return);
    RTS;
_sx_reader:
    DK_ENTER(3);
    DK_SAVE(2, R3);
    R0 = I3; DK_SAVE(3, R0);
    R0 = _sx_cursor; R0 = LSHIFT R0 BY -2; I3 = R0;
    DK_SAMPLE_BANK_LOCK(R3);
    DK_SAVE(4, R3);
    DK_SAMPLE_BANK_CHECK_ADMISSION(_sx_open, _sx_closed, _sx_count, R0, R1, _sx_denied, _sx_faulted);
    DK_SAMPLE_BANK_CHECK_EPOCH(_sx_token, I3, 6, R0, R1, _sx_denied);
    DK_SAMPLE_BANK_ACQUIRE(_sx_count, R0);
    // Unsafe-control reader retains the real admission/count pin, but does not
    // hold exclusion during PCM reads. Its writer must still respect drain.
    R0 = DM(_sx_unlocked_control); R0 = PASS R0;
    IF EQ JUMP _sx_read;
    DK_SAMPLE_BANK_UNLOCK(R3);
_sx_read:
    R4 = _sx_sources; R4 = LSHIFT R4 BY -2;
    R8 = _sx_outputs; R8 = LSHIFT R8 BY -2;
    R12 = I3;
    DK_CALL(_dk_sample_pcm16_stereo_linear, _sx_after_pcm);
_sx_after_pcm:
    R0 = DM(_sx_unlocked_control); R0 = PASS R0;
    IF EQ JUMP _sx_release;
    DK_SAMPLE_BANK_LOCK(R0);
_sx_release:
    DK_SAMPLE_BANK_RELEASE(_sx_count, R0, _sx_faulted, _sx_released);
_sx_released:
    R0 = 0;
    JUMP _sx_exit;
_sx_faulted:
    DK_SAMPLE_BANK_FAIL_CLOSED(_sx_fault, _sx_closed, _sx_open, _sx_ready, R0);
    DK_SAMPLE_BANK_POISON_COUNT(_sx_count, R0);
_sx_denied:
    I4 = 134438912;
    R0 = 0; R1 = 64;
_sx_zero:
    DM(I4, M6) = R0;
    R1 = R1 - 1; R1 = PASS R1; IF GT JUMP _sx_zero;
    R0 = 1;
_sx_exit:
    DK_RESTORE(R3, 4);
    DK_SAMPLE_BANK_UNLOCK(R3);
    DK_RETADDR;
    DK_RESTORE(R3, 2);
    DK_RESTORE(I3, 3);
    NOP; NOP;
    DK_LEAVE(R1, 4);

// Stand-alone test writer. R4/R8 = physical affected byte start/length.
// R12=0 is the real CLOSE + exact-zero drain writer; R12=1 explicitly selects
// the UNSAFE invalidation-only shortcut. Neither publishes a successor.
.GLOBAL _sx_writer;
_sx_writer:
    DK_SAMPLE_BANK_LOCK(R7);
    R0 = DM(_sx_span); R1 = DM(_sx_span_1);
    R2 = R4 + R8;
    COMP(R2, R0); IF LE JUMP _sx_writer_miss;
    COMP(R4, R1); IF GE JUMP _sx_writer_miss;
    DK_SAMPLE_BANK_CLOSE(_sx_open, _sx_ready, _sx_closed, R0);
    R0 = PASS R12; IF NE JUMP _sx_UNSAFE_invalidation_only;
    DK_SAMPLE_BANK_CHECK_DRAIN(_sx_count, _sx_fault, R0, R1, _sx_writer_busy);
_sx_UNSAFE_invalidation_only:
    R0 = DM(_sx_count); DM(_sx_write_count) = R0;
    R0 = _sx_sources; R0 = LSHIFT R0 BY -2; I4 = R0;
    R4 = DM(0, I4); R8 = DM(1, I4);
    I4 = R4; R0 = 0x30002000; R1 = 64;
_sx_fill_left:
    DM(I4, M6) = R0;
    R1 = R1 - 1; R1 = PASS R1; IF GT JUMP _sx_fill_left;
    I4 = R8; R0 = 0x9000a000; R1 = 64;
_sx_fill_right:
    DM(I4, M6) = R0;
    R1 = R1 - 1; R1 = PASS R1; IF GT JUMP _sx_fill_right;
    R0 = 1; DM(_sx_written) = R0;
_sx_writer_miss:
    R0 = 0; JUMP _sx_writer_exit;
_sx_writer_busy:
    R0 = 2;
_sx_writer_exit:
    DK_SAMPLE_BANK_UNLOCK(R7);
    RTS;
