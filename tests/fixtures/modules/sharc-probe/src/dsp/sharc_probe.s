// test/sharc-probe: an effect of the FX contract (<dk/fx.h>) that measures RECIPS and RSQRTS on the real core and reports through the module step word.
//
// The audio passes unchanged (the block is copied). Each block the routine stores ONE 32-bit word in `DM(_dk_step)`, diagnostic word 25 of the DSP reply
// (`core/dsp-link`), which the ColdFire reads: the block tail copies it into that block's record, so every word arrives once, at 1500 words a second
// (modules/core/adapters/dsp-link/README.md). The words form frames: a header (0xB0, payload word count 12, frame number), 12 payload words (4 records of 3
// words) and a checksum. The format is in tools/device/dkprobe_seq.py. A frame is built in one block, then its 14 words go out one per block.
//
// A run is PR_NRECORDS records: record 0 (run counter, items, checksum of the input table), record 1 (MODE1, ASTAT, STKY as found), one record per item
// (result bits, ASTAT after, STKY after), padding. Before each item ASTAT bits 0 to 13 and the whole STKY are cleared; ASTAT and STKY are put back at the end
// of the frame. MODE1 is read, never written. The table `_pr_tab` (probe_seq.inc) holds the inputs, the first PR_NRECIP for RECIPS and the rest for RSQRTS.
//
// The two-source checksum steps never write a register that they read in the same instruction (the 16-bit forms of the build's check); the loops take two words
// per pass for that reason (the table has an even number of items).

#include "dk_fx.inc"
#include "probe_seq.inc"

.EXTERN _dk_step;

.SECTION/PM seg_pmco;
.GLOBAL _pr_process;

_pr_process:                              // R4 = state, R8 = dk_fx_in, R12 = parameter words (none)
    I2 = R8;
    I0 = DM(DK_FX_IN_W_IN, I2);
    I1 = DM(DK_FX_IN_W_OUT, I2);
    LCNTR = DK_FX_BLOCK, DO _pr_cp UNTIL LCE;
        R0 = DM(I0, M6);
_pr_cp: DM(I1, M6) = R0;
    R0 = DM(_pr_left);
    R0 = PASS R0;
    IF NE JUMP _pr_emit;

    // ---- the frame is used up: build the next one ----
    R14 = ASTAT;                          // as found, put back at the end
    R15 = STKY;
    R11 = 0xffffc000;
    R11 = R14 AND R11;                    // ASTAT with bits 0 to 13 cleared
    R12 = 0;                              // STKY cleared
    I1 = _pr_buf;
    R0 = DM(_pr_seq);
    R2 = 0xffff;
    R0 = R0 AND R2;
    R1 = 0xb00c0000;
    R1 = R1 OR R0;                        // the header word
    DM(I1, M6) = R1;
    I0 = DM(_pr_tp);                      // the next input of the table
    R13 = DM(_pr_rec);                    // the record of the run that comes next
    R6 = 4;                               // records of a frame
_pr_rec_loop:
    R0 = PASS R13;
    IF EQ JUMP _pr_r0;
    R0 = R13 - 1;
    IF EQ JUMP _pr_r1;
    R0 = PR_NITEMS + 2;
    COMP(R13, R0);
    IF GE JUMP _pr_pad;
    R0 = PR_NRECIP + 2;
    COMP(R13, R0);
    IF GE JUMP _pr_rs;
    R1 = DM(I0, M6);                      // RECIPS item
    ASTAT = R11;
    STKY = R12;
    NOP;
    NOP;
    F0 = RECIPS F1;
    NOP;
    NOP;
    R3 = ASTAT;
    R4 = STKY;
    JUMP _pr_put;
_pr_rs:
    R1 = DM(I0, M6);                      // RSQRTS item
    ASTAT = R11;
    STKY = R12;
    NOP;
    NOP;
    F0 = RSQRTS F1;
    NOP;
    NOP;
    R3 = ASTAT;
    R4 = STKY;
    JUMP _pr_put;
_pr_r0:                                   // record 0: run counter, items, checksum of the table (s1 ^ s2, s1 += w, s2 += s1)
    I0 = _pr_tab;                         // a run starts: the first input
    I2 = _pr_tab;
    R8 = 0;
    R9 = 0;
    LCNTR = PR_NITEMS / 2, DO _pr_ts UNTIL LCE;
        R5 = DM(I2, M6);
        R7 = R8 + R5;
        R10 = R9 + R7;
        R5 = DM(I2, M6);
        R8 = R7 + R5;
_pr_ts: R9 = R10 + R8;
    R4 = R8 XOR R9;
    R0 = DM(_pr_run);
    R3 = PR_NITEMS;
    JUMP _pr_put;
_pr_r1:                                   // record 1: the state as found
    R0 = MODE1;
    R3 = PASS R14;
    R4 = PASS R15;
    JUMP _pr_put;
_pr_pad:
    R0 = -1;
    R3 = -1;
    R4 = -1;
_pr_put:
    DM(I1, M6) = R0;
    DM(I1, M6) = R3;
    DM(I1, M6) = R4;
    R13 = R13 + 1;
    R6 = R6 - 1;
    IF NE JUMP _pr_rec_loop;

    // ---- the checksum of the 13 words (the header, then 12 payload words in pairs) ----
    I2 = _pr_buf;
    R8 = DM(I2, M6);                      // s1 = s2 = the header
    R9 = PASS R8;
    LCNTR = 6, DO _pr_cs UNTIL LCE;
        R5 = DM(I2, M6);
        R7 = R8 + R5;
        R10 = R9 + R7;
        R5 = DM(I2, M6);
        R8 = R7 + R5;
_pr_cs: R9 = R10 + R8;
    R0 = R8 XOR R9;
    DM(I1, M6) = R0;
    // ---- state for the next frame ----
    R0 = DM(_pr_seq);
    R0 = R0 + 1;
    DM(_pr_seq) = R0;
    R0 = I0;
    DM(_pr_tp) = R0;
    R0 = PR_NRECORDS;
    COMP(R13, R0);
    IF LT JUMP _pr_keep;
    R13 = 0;                              // the run is over: the next frame starts the next run
    R0 = DM(_pr_run);
    R0 = R0 + 1;
    DM(_pr_run) = R0;
_pr_keep:
    DM(_pr_rec) = R13;
    R0 = _pr_buf;
    DM(_pr_bp) = R0;
    R0 = 14;
    DM(_pr_left) = R0;
    STKY = R15;                           // put back what we found
    ASTAT = R14;

_pr_emit:                                 // one word of the frame goes out
    I3 = DM(_pr_bp);
    R2 = DM(I3, M6);
    R0 = I3;
    DM(_pr_bp) = R0;
    R0 = DM(_pr_left);
    R0 = R0 - 1;
    DM(_pr_left) = R0;
    DM(_dk_step) = R2;
    R0 = DK_FX_OK;
    JUMP (M13, I13);

.SECTION/DM seg_dmda;
// dk_fx_desc: magic, contract, n_params, state_bytes, max_cycles_block, max_call_depth, stack_bytes.
.GLOBAL _pr_desc;
.VAR _pr_desc = DK_FX_MAGIC;
.VAR _pr_desc1 = DK_FX_V1;
.VAR _pr_desc2 = 0;
.VAR _pr_desc3 = 4;
.VAR _pr_desc4 = 40000;
.VAR _pr_desc5 = 0;
.VAR _pr_desc6 = 0;
// the sequence state: words left in the frame buffer, the next word out, the next frame number, the next record of the run, the next table input, the run
.VAR _pr_left = 0;
.VAR _pr_bp = 0;
.VAR _pr_seq = 0;
.VAR _pr_rec = 0;
.VAR _pr_tp = 0;
.VAR _pr_run = 1;
.VAR _pr_buf = 0;
.VAR _pr_buf1 = 0;
.VAR _pr_buf2 = 0;
.VAR _pr_buf3 = 0;
.VAR _pr_buf4 = 0;
.VAR _pr_buf5 = 0;
.VAR _pr_buf6 = 0;
.VAR _pr_buf7 = 0;
.VAR _pr_buf8 = 0;
.VAR _pr_buf9 = 0;
.VAR _pr_buf10 = 0;
.VAR _pr_buf11 = 0;
.VAR _pr_buf12 = 0;
.VAR _pr_buf13 = 0;
