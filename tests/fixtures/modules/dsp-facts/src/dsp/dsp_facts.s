// test/dsp-facts: an effect of the FX contract (<dk/fx.h>) that measures the SHARC+ core it runs on and reports through the module step word.
//
// The audio passes unchanged (the block is copied). Each block the routine stores ONE 32-bit word in `DM(_dk_step)`, diagnostic word 25 of the DSP reply
// (`core/dsp-link`), which the ColdFire collector keeps (modules/tools/probe). The words form frames exactly as in test/sharc-probe: a header (0xB0, payload
// word count 12, frame number), 12 payload words (4 records of 3 words) and a checksum; a frame is built in one block and its 14 words go out one per block.
//
// The records, and what each item measures, are defined in tools/device/dkprobe_facts.py. That file also writes `facts_seq.inc`: the table constants and one
// routine per frame (`_fc_f0` ...), each building the 4 records of its frame through I5 and ending with a jump to `_fc_fin`. This file holds the common parts:
// the call prologue (the cycle counter at every call: the "block" records), the frame dispatch, the frame tail (checksum, state) and the word output.
//
// Registers: R0-R15, I0-I3, I5 and M1-M4 only, M5 = 0, M6 = 1 as the contract pins them. ASTAT and STKY are saved at the start of a frame and written back at its
// end (the items clear them before a measurement); MODE1 is changed only by the saturation items and written back at once.

#include "dk_fx.inc"
#include "facts_seq.inc"

.EXTERN _dk_step;

.SECTION/PM seg_pmco;
.GLOBAL _fc_process;

_fc_process:                              // R4 = state, R8 = dk_fx_in, R12 = parameter words (none)
    R0 = EMUCLK;                          // the cycle counter at this call, and the two deltas before it
    R1 = DM(_fc_prev1);
    R2 = DM(_fc_prev2);
    R3 = R0 - R1;
    R5 = R1 - R2;
    DM(_fc_t0) = R0;
    DM(_fc_d1) = R3;
    DM(_fc_d2) = R5;
    DM(_fc_prev2) = R1;
    DM(_fc_prev1) = R0;
    DM(_fc_state) = R4;                   // the cache test buffer
    I2 = R8;
    I0 = DM(DK_FX_IN_W_IN, I2);
    I1 = DM(DK_FX_IN_W_OUT, I2);
    LCNTR = DK_FX_BLOCK, DO _fc_cp UNTIL LCE;
        R0 = DM(I0, M6);
_fc_cp: DM(I1, M6) = R0;
    R0 = DM(_fc_left);
    R0 = PASS R0;
    IF NE JUMP _fc_emit;

    // ---- the frame is used up: build the next one ----
    R0 = ASTAT;                           // as found, put back at the end
    DM(_fc_astat0) = R0;
    R0 = STKY;
    DM(_fc_stky0) = R0;
    I5 = _fc_buf;
    R0 = DM(_fc_seq);
    R2 = 0xffff;
    R0 = R0 AND R2;
    R1 = 0xb00c0000;
    R1 = R1 OR R0;                        // the header word
    DM(I5, M6) = R1;
    JUMP _fc_ladder;                      // the frame of the run: its routine (generated) builds the records and jumps to _fc_fin

_fc_fin:                                  // I5 = the end of the 13 words; the checksum of the header and the 12 payload words, in pairs
    I2 = _fc_buf;
    R8 = DM(I2, M6);                      // s1 = s2 = the header
    R9 = PASS R8;
    LCNTR = 6, DO _fc_cs UNTIL LCE;
        R5 = DM(I2, M6);
        R7 = R8 + R5;
        R10 = R9 + R7;
        R5 = DM(I2, M6);
        R8 = R7 + R5;
_fc_cs: R9 = R10 + R8;
    R0 = R8 XOR R9;
    DM(I5, M6) = R0;
    R0 = DM(_fc_seq);
    R0 = R0 + 1;
    DM(_fc_seq) = R0;
    R0 = DM(_fc_fr);
    R0 = R0 + 1;
    R1 = FC_NFRAMES;
    COMP(R0, R1);
    IF LT JUMP _fc_keep;
    R0 = 0;                               // the run is over: the next frame starts the next run
    R1 = DM(_fc_run);
    R1 = R1 + 1;
    DM(_fc_run) = R1;
_fc_keep:
    DM(_fc_fr) = R0;
    R0 = _fc_buf;
    DM(_fc_bp) = R0;
    R0 = 14;
    DM(_fc_left) = R0;
    R0 = DM(_fc_stky0);                   // put back what we found
    STKY = R0;
    R0 = DM(_fc_astat0);
    ASTAT = R0;

_fc_emit:                                 // one word of the frame goes out
    I3 = DM(_fc_bp);
    R2 = DM(I3, M6);
    R0 = I3;
    DM(_fc_bp) = R0;
    R0 = DM(_fc_left);
    R0 = R0 - 1;
    DM(_fc_left) = R0;
    DM(_dk_step) = R2;
    R0 = DK_FX_OK;
    JUMP (M13, I13);

.SECTION/DM seg_dmda;
// dk_fx_desc: magic, contract, n_params, state_bytes, max_cycles_block, max_call_depth, stack_bytes.
.GLOBAL _fc_desc;
.VAR _fc_desc = DK_FX_MAGIC;
.VAR _fc_desc1 = DK_FX_V1;
.VAR _fc_desc2 = 0;
.VAR _fc_desc3 = FC_STATE_BYTES;
.VAR _fc_desc4 = 120000;
.VAR _fc_desc5 = 0;
.VAR _fc_desc6 = 0;
// the call stamps, the stream state, the saved flags, the statistics of the item that runs
.VAR _fc_prev1 = 0;
.VAR _fc_prev2 = 0;
.VAR _fc_t0 = 0;
.VAR _fc_d1 = 0;
.VAR _fc_d2 = 0;
.VAR _fc_state = 0;
.VAR _fc_left = 0;
.VAR _fc_bp = 0;
.VAR _fc_seq = 0;
.VAR _fc_fr = 0;
.VAR _fc_run = 1;
.VAR _fc_astat0 = 0;
.VAR _fc_stky0 = 0;
.VAR _fc_mode1 = 0;
.VAR _fc_min = 0;
.VAR _fc_first = 0;
.VAR _fc_max = 0;
.VAR _fc_reps = 0;
.VAR _fc_buf = 0;
.VAR _fc_buf1 = 0;
.VAR _fc_buf2 = 0;
.VAR _fc_buf3 = 0;
.VAR _fc_buf4 = 0;
.VAR _fc_buf5 = 0;
.VAR _fc_buf6 = 0;
.VAR _fc_buf7 = 0;
.VAR _fc_buf8 = 0;
.VAR _fc_buf9 = 0;
.VAR _fc_buf10 = 0;
.VAR _fc_buf11 = 0;
.VAR _fc_buf12 = 0;
.VAR _fc_buf13 = 0;
