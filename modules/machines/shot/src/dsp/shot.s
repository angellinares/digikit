// digikit/shot: one-shot playback of the stereo sample bound to the track, a stereo machine of the SDK contract (<dk/machine.h>, contract 1.2),
// one body for both devices. The adapter of core/dsp-voice calls `_shot_note_on` once per note and `_shot_render` once per block with
// R4 = this voice's state, R8 = the `dk_voice_in` (pitch in semitones << 8 with the tune folded in, the track, the outputs `out` (L) and `out_r` (R)),
// R12 = the five parameter words SMP, START, END, REVERSE, PITCH (unit << 8: 0 .. 0x7f00 = UI 0 .. 127; SMP is 0 .. 15, 0 .. 0x0f00).
//
// The sample is a pool entry's `dk_strack` record (<dk/sample_track.h>), written by the runtime into `_shot_rec` (16 records of 8 words, one for
// each pool index; the control sets of 4 words are in `_shot_ctl`). `note_on` takes the pool index from the SMP word (word >> 8, so a p-lock of
// SMP picks the entry of that step) and, in one masked scope, checks the admission (the record
// is ready, not closed, count in range), latches the epoch (the record's token) in the voice and acquires the track. It copies the plane
// bases, the frame count, the channels and the source rate, releases the track and sets up the cursor. Every `render` checks the admission and
// the cached epoch in one masked scope, acquires the track, reads, and releases it. A refused admission or a stale token (select, unbind, a
// stock reset) makes the block silent and ends the voice with DK_DONE; the PCM is not touched. The status always carries DK_STEREO.
//
// Parameters (latched at note-on: SMP, START, END, REVERSE; read every block: PITCH):
//   SMP          pool index 0 .. 15 (the word is index << 8). An empty or closed entry is silent: the voice ends. A word above 15 << 8 is silent too.
//   START, END   frame = round(frames * word / 0x7f00), clamped to 0 .. frames: 0 .. 127 spread over the sample. END <= START plays nothing.
//   REVERSE      word >= 0x4000 (UI 64 and above): play backwards from END - 1 down to START.
//   PITCH        UI 64 (0x4000) is the played note unchanged, each UI unit is 3/8 semitone: -24 .. +23.625 semitones.
// The cursor rate (Q16.16 frames per output sample) is srcrate / 48000 * 2^((pitch / 256 - 60 + (PITCH - 0x4000) * 3 / 2048) / 12), clamped to
// 1 / 65536 .. 64 frames per sample, negative for REVERSE. MIDI note 60 plays the sample at its own speed. A mono sample (channels 1) uses
// base_r = base_l: the same samples go to both outputs (centred). The sample plays once, then the voice ends.
//
// State per voice (18 words, all cleared at note-on): 0 .. 5 the cursor of the kernel (start, end, index, fraction, rate, done), 6 the epoch (token),
// 7 base_l, 8 base_r, 11 the source rate in Hz, 12 the pool index, 13 reverse, 14 the adapter's return address (I13) across the kernel call,
// 16 out, 17 out_r (the output pointer pair for the stereo kernel).

#include "dk_machine.inc"
#include "dsp/call.inc"
#include "dsp/sample.inc"
#include "dsp/sample_pcm16.inc"
#include "dsp/sample_pcm16_stereo.inc"
#include "dsp/sample_bank.inc"
#include "dsp/sample_track.inc"

// I5 = the record and I1 = the control set of the track in R0 (normal-word addresses); the DAG registers are ready 3 instructions later.
// Clobbers R1, R2.
#define SHOT_TRACK_PTRS \
    R1 = _shot_rec; \
    R1 = LSHIFT R1 BY -2; \
    R2 = LSHIFT R0 BY 3; \
    R1 = R1 + R2; \
    I5 = R1; \
    R1 = _shot_ctl; \
    R1 = LSHIFT R1 BY -2; \
    R2 = LSHIFT R0 BY 2; \
    R1 = R1 + R2; \
    I1 = R1

.SECTION/PM seg_pmco;
.GLOBAL _shot_render;
.GLOBAL _shot_note_on;
.EXTERN _dv_exp2;

_shot_render:
    I3 = R4;                              // the state
    I2 = R8;                              // the voice input
    I4 = R12;                             // the parameter words
    NOP; NOP; NOP;
    R0 = I13;
    DM(14, I3) = R0;                      // the adapter's return address (the kernel call may use I12; I13 is kept here in any case)
    // the cursor rate: srcrate / 48000 * 2^x, x = ((pitch / 256 - 60) + (PITCH - 0x4000) * 3 / 2048) / 12 in 2^-8 .. 2^8
    R0 = DM(DK_IN_W_PITCH, I2);
    F10 = FLOAT R0;
    R1 = 0x3b800000;                      // 1 / 256
    F10 = F10 * F1;
    R1 = 0x42700000;                      // 60.0
    F10 = F10 - F1;
    R0 = DM(4, I4);                       // PITCH
    R1 = 0x4000;
    R0 = R0 - R1;
    F0 = FLOAT R0;
    R1 = 0x3ac00000;                      // 3 / 2048
    F0 = F0 * F1;
    F10 = F10 + F0;
    R1 = 0x3daaaaab;                      // 1 / 12
    F10 = F10 * F1;
    R1 = 0x41000000;                      // 8.0
    F10 = MIN(F10, F1);
    R1 = 0xc1000000;                      // -8.0
    F10 = MAX(F10, F1);
    F0 = PASS F10;
    CALL _dv_exp2;
    R1 = DM(11, I3);                      // the source rate in Hz
    F1 = FLOAT R1;
    F0 = F0 * F1;
    R1 = 0x3faec33e;                      // 65536 / 48000
    F0 = F0 * F1;
    R1 = 0x4a800000;                      // 64.0 frames per sample in Q16.16
    F0 = MIN(F0, F1);
    R1 = 0x3f800000;                      // 1.0 (1 / 65536 frames per sample)
    F0 = MAX(F0, F1);
    R0 = FIX F0;
    R1 = DM(13, I3);                      // REVERSE
    R1 = PASS R1;
    IF NE R0 = -R0;
    DM(4, I3) = R0;
    R0 = DM(12, I3);                      // the pool index
    SHOT_TRACK_PTRS;
    NOP; NOP; NOP;
    DK_SAMPLE_BANK_LOCK(R13);
    DK_STRACK_CHECK_ADMISSION(I5, I1, R0, R1, _shot_render_denied, _shot_render_fault);
    DK_STRACK_CHECK_EPOCH(I5, I3, 6, R0, R1, _shot_render_denied);
    DK_STRACK_ACQUIRE(I1, R0);
    DK_SAMPLE_BANK_UNLOCK(R13);
.GLOBAL _shot_render_acquired;
_shot_render_acquired:
    // One stereo read: R4 = the plane bases [base_l, base_r] (state words 7, 8), R8 = the output pointers [out, out_r] (state words 16, 17),
    // R12 = the cursor (state words 0 .. 5). The pointers are byte addresses of the state, which is how the adapters enter (finding 111).
    R0 = DM(DK_IN_W_OUT, I2);
    DM(16, I3) = R0;
    R0 = DM(DK_IN_W_OUT_R, I2);
    DM(17, I3) = R0;
    MODIFY(I7, -1) (NW);                  // the adapter's frame ends at I7: one word for DK_CALL's second store
    R12 = I3;                             // the cursor
    R0 = 28;                              // word 7 is 28 bytes on
    R4 = R12 + R0;
    R0 = 64;                              // word 16 is 64 bytes on
    R8 = R12 + R0;
    DK_CALL(_dk_sample_pcm16_stereo_linear, _shot_after_stereo);
    MODIFY(I7, 1) (NW);
    DK_SAMPLE_BANK_LOCK(R13);
    DK_STRACK_RELEASE(I1, R0, _shot_render_release_fault, _shot_render_released);
_shot_render_release_fault:
    DK_STRACK_FAIL_CLOSED(I5, I1, R0);
    DK_STRACK_POISON_COUNT(I1, R0);
_shot_render_released:
    DK_SAMPLE_BANK_UNLOCK(R13);
    R0 = DM(5, I3);                       // done: DK_DONE (1) or DK_OK (0)
    R1 = DK_STEREO;
    R0 = R0 OR R1;
    I13 = DM(14, I3);
    NOP; NOP; NOP;
    JUMP (M13, I13);
_shot_render_fault:
    DK_STRACK_FAIL_CLOSED(I5, I1, R0);
    DK_STRACK_POISON_COUNT(I1, R0);
_shot_render_denied:
    DK_SAMPLE_BANK_UNLOCK(R13);
    R1 = DM(DK_IN_W_OUT, I2);             // a silent block on both outputs, the voice ends
    I0 = R1;
    R1 = DM(DK_IN_W_OUT_R, I2);
    I4 = R1;
    R0 = 0;
    NOP; NOP; NOP;
    LCNTR = 32, DO _shot_zero UNTIL LCE;
        DM(I4, M6) = R0;
_shot_zero: DM(I0, M6) = R0;
    R0 = 1;
    DM(5, I3) = R0;
    I13 = DM(14, I3);
    R0 = DK_DONE;
    R1 = DK_STEREO;
    R0 = R0 OR R1;
    NOP; NOP; NOP;
    JUMP (M13, I13);

// (not at offset 0 of the image: an offset of 0 reads as "no note_on" in the manifest and the pack)
_shot_note_on:
    I3 = R4;                              // the state (cleared by the adapter)
    I2 = R8;                              // the voice input
    I4 = R12;                             // the parameter words
    NOP; NOP; NOP;
    R0 = 1;
    DM(5, I3) = R0;                       // done until the sample is bound and acquired
    R0 = DM(0, I4);                       // SMP word: pool index << 8
    R0 = LSHIFT R0 BY -8;
    R1 = 15;
    COMP(R0, R1);
    IF GT JUMP _shot_note_return;        // not an entry of the pool: no epoch, render ends the voice
    DM(12, I3) = R0;
    SHOT_TRACK_PTRS;
    NOP; NOP; NOP;
    DK_SAMPLE_BANK_LOCK(R13);
    DK_STRACK_CHECK_ADMISSION(I5, I1, R0, R1, _shot_note_denied, _shot_note_fault);
    DK_STRACK_LATCH_EPOCH(I5, I3, 6, R0, R1, _shot_note_denied);
    DK_STRACK_ACQUIRE(I1, R0);
    DK_SAMPLE_BANK_UNLOCK(R13);
.GLOBAL _shot_note_acquired;
_shot_note_acquired:
    R0 = DM(2, I5);                       // frames
    R1 = DM(3, I5);                       // source rate
    R2 = DM(4, I5);                       // channels
    R3 = DM(5, I5);                       // fmt
    R9 = DM(6, I5);                       // base_l
    R10 = DM(7, I5);                      // base_r
    DM(7, I3) = R9;
    DM(11, I3) = R1;
    R1 = 1;
    COMP(R2, R1);
    IF EQ R10 = PASS R9;                  // mono: both outputs read the one plane
    DM(8, I3) = R10;
    R1 = 1;                               // DK_STRACK_FMT_PCM16
    COMP(R3, R1);
    IF NE JUMP _shot_note_release;       // not PCM16: stay done
    R9 = DM(1, I4);                       // START word
    R10 = DM(2, I4);                      // END word
    F11 = FLOAT R0;                       // frames
    R1 = 0x38010204;                      // 1 / 32512
    F2 = FLOAT R9;
    F2 = F2 * F11;
    F2 = F2 * F1;
    R2 = FIX F2;
    R2 = MIN(R2, R0);                     // start frame, 0 .. frames
    F3 = FLOAT R10;
    F3 = F3 * F11;
    F3 = F3 * F1;
    R3 = FIX F3;
    R3 = MIN(R3, R0);                     // end frame
    DM(0, I3) = R2;
    DM(1, I3) = R3;
    COMP(R2, R3);
    IF GE JUMP _shot_note_release;       // empty: stay done
    DM(2, I3) = R2;                       // forward: the first frame is START
    R0 = DM(3, I4);                       // REVERSE word
    R1 = 0x4000;
    COMP(R0, R1);
    IF LT JUMP _shot_note_forward;
    R0 = R3 - 1;
    DM(2, I3) = R0;                       // reverse: the first frame is END - 1
    R0 = 1;
    DM(13, I3) = R0;
_shot_note_forward:
    R0 = 0;
    DM(5, I3) = R0;                       // playing
_shot_note_release:
    DK_SAMPLE_BANK_LOCK(R13);
    DK_STRACK_RELEASE(I1, R0, _shot_note_release_fault, _shot_note_released);
_shot_note_release_fault:
    DK_STRACK_FAIL_CLOSED(I5, I1, R0);
    DK_STRACK_POISON_COUNT(I1, R0);
_shot_note_released:
    DK_SAMPLE_BANK_UNLOCK(R13);
    JUMP _shot_note_return;
_shot_note_fault:
    DK_STRACK_FAIL_CLOSED(I5, I1, R0);
    DK_STRACK_POISON_COUNT(I1, R0);
_shot_note_denied:
    DK_SAMPLE_BANK_UNLOCK(R13);
_shot_note_return:
    R0 = 0;
    NOP;
    JUMP (M13, I13);

.SECTION/DM seg_dmda;
// The pool table (<dk/sample_track.h>): the runtime fills the records and moves the control words; the module only reads them.
.PKDATA _shot_rec, 512;                  // 16 dk_strack records of 8 words, one for each pool index
.PKDATA _shot_ctl, 256;                  // 16 control sets of 4 words: closed, count, fault, reserved
// dk_machine_desc: magic, contract (machine contract 1.2: stereo), n_params, voices, state_bytes, state_clear_bytes, max_cycles_block,
// max_cycles_note_on, max_call_depth, stack_bytes. Cycles are instruction ceilings: the stereo kernel's 4,396 with its call, the rate (exp2 and
// about 60 more) and the two masked scopes; note-on adds about 150 and one render.
.GLOBAL _shot_desc;
.VAR _shot_desc = DK_MAGIC;
.VAR _shot_desc1 = 257;                  // DK_MACHINE_V1 | DK_MACHINE_STEREO (an expression is not accepted here)
.VAR _shot_desc2 = 5;
.VAR _shot_desc3 = DK_MAX_VOICES;
.VAR _shot_desc4 = 72;
.VAR _shot_desc5 = 72;
.VAR _shot_desc6 = 4800;
.VAR _shot_desc7 = 5000;
.VAR _shot_desc8 = 3;
.VAR _shot_desc9 = 192;
