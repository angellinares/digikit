// core/fx send route, the part both devices share (fxsend.s: Digitakt II, fxsend-dn2.s: Digitone II): the entry, the row loop, the send bus clear and the 16 gains.
// The wrappers hold the device's header comment, its externs, the mix of the tracks or slots (fxsend-tracks.inc, fxsend-slots.inc) and the data of that mix.

#include "dk_fx.inc"

#define FX_PC 0
#define FX_EN 3
#define FX_WAS 12
#define FX_MIX 13
#define FX_DIS 14
#define FX_ROUTE 18
#define FX_AMT0_BYTES 76                  // word 19
#define FX_ROW_BYTES 96
#define FXS_TRACKS 16
#define FXS_TRACK_BYTES 256               // a track bus: 64 floats
#define FXS_RIGHT_BYTES 128               // the right channel of a bus of 32 + 32 floats

.SECTION/PM seg_pmco;
.GLOBAL _fxs_entry;
.EXTERN _fx_table;
.EXTERN _fx_enter;
.EXTERN _fx_leave;
.EXTERN _fx_exec;
.EXTERN _fx_c;
.EXTERN _fx_in;
.EXTERN _fx_scratch;
.EXTERN _fx_s_r0;
.EXTERN _fx_s_i4;
.EXTERN _fx_s_l4;
.EXTERN _fxs_return;
.EXTERN _fxs_back;

_fxs_entry:
    DM(_fx_s_r0) = R0;
    R0 = I4;
    DM(_fx_s_i4) = R0;
    R0 = L4;
    DM(_fx_s_l4) = R0;
    L4 = 0;
    I4 = _fx_table;
    // the quick scan: rows 0 to 3. pc 0 ends the table, pc 0xffffffff is a free row (next row), route 1 is a send row: run, anything else: next row.
    R0 = DM(0, I4);
    R0 = PASS R0;
    IF EQ JUMP _fxs_none;
    R0 = R0 + 1;
    IF EQ JUMP _fxs_q1;
    R0 = DM(18, I4);
    R0 = R0 - 1;
    IF EQ JUMP _fxs_run;
_fxs_q1:
    R0 = DM(24, I4);
    R0 = PASS R0;
    IF EQ JUMP _fxs_none;
    R0 = R0 + 1;
    IF EQ JUMP _fxs_q2;
    R0 = DM(42, I4);
    R0 = R0 - 1;
    IF EQ JUMP _fxs_run;
_fxs_q2:
    R0 = DM(48, I4);
    R0 = PASS R0;
    IF EQ JUMP _fxs_none;
    R0 = R0 + 1;
    IF EQ JUMP _fxs_q3;
    R0 = DM(66, I4);
    R0 = R0 - 1;
    IF EQ JUMP _fxs_run;
_fxs_q3:
    R0 = DM(72, I4);
    R0 = PASS R0;
    IF EQ JUMP _fxs_none;
    R0 = R0 + 1;
    IF EQ JUMP _fxs_none;
    R0 = DM(90, I4);
    R0 = R0 - 1;
    IF EQ JUMP _fxs_run;
_fxs_none:
    R0 = DM(_fx_s_l4);
    L4 = R0;
    R0 = DM(_fx_s_i4);
    I4 = R0;
    R0 = DM(_fx_s_r0);
    JUMP _fxs_back;                       // the displaced instruction, then the master function

_fxs_run:
    CALL _fx_enter;
    I4 = _fx_in;
    R0 = _fxs_bus;
    R1 = 4;
    R0 = R0 + R1;                         // the bus: one word behind the pad
    DM(DK_FX_IN_W_IN, I4) = R0;
    R0 = _fx_table;
    DM(_fx_c + 0) = R0;                   // the row
    R0 = 0;
    DM(_fxs_c + 0) = R0;                  // the row's index

_fxs_row:
    R0 = DM(_fx_c + 0);
    I4 = R0;
    R0 = DM(FX_PC, I4);
    R0 = PASS R0;
    IF EQ JUMP _fxs_done;
    R0 = R0 + 1;
    IF EQ JUMP _fxs_skip;                 // a free row
    R0 = DM(FX_ROUTE, I4);
    R0 = R0 - 1;
    IF NE JUMP _fxs_skip;                 // not a send row
    R0 = DM(FX_DIS, I4);
    R0 = PASS R0;
    IF NE JUMP _fxs_skip;                 // switched off by its faults
    R0 = DM(FX_EN, I4);
    R1 = DM(FX_WAS, I4);
    R0 = R0 OR R1;
    IF EQ JUMP _fxs_skip;                 // off and was off: no bus to build
    CALL _fxs_mixbus;
    R0 = DM(_fx_c + 0);
    I4 = R0;
    CALL _fx_exec;
    R0 = PASS R0;
    IF EQ JUMP _fxs_skip;                 // nothing to add: off, switched off or a fault
    CALL _fxs_ret;
    R0 = DM(_fx_c + 0);
    I4 = R0;
    R1 = DM(_fx_c + 8);
    DM(FX_WAS, I4) = R1;
_fxs_skip:
    R0 = DM(_fx_c + 0);
    R1 = FX_ROW_BYTES;
    R0 = R0 + R1;
    DM(_fx_c + 0) = R0;
    R0 = DM(_fxs_c + 0);
    R0 = R0 + 1;
    DM(_fxs_c + 0) = R0;
    JUMP _fxs_row;

_fxs_done:
    CALL _fx_leave;
    JUMP _fxs_none;

// The send bus of the row at `_fx_c + 0` (row number `_fxs_c + 0`): the sum over the tracks of gain * the track's bus. Clobbers what an effect may.
_fxs_mixbus:
    R0 = _fxs_bus;
    R1 = 4;
    R0 = R0 + R1;
    I0 = R0;
    R0 = 0;
    LCNTR = DK_FX_BLOCK, DO _fxs_z UNTIL LCE;
_fxs_z: DM(I0, M6) = R0;
    // the 16 new gains: (byte / 127) squared, from the amount words 19 to 22
    R0 = DM(_fx_c + 0);
    R1 = FX_AMT0_BYTES;
    R0 = R0 + R1;
    I1 = R0;
    I0 = _fxs_gnew;
    R4 = 127;
    R5 = -8;
    R6 = 0xff;
    R7 = 0x3c010204;                      // 1 / 127
    LCNTR = 4, DO _fxs_cw UNTIL LCE;
        R0 = DM(I1, M6);
        LCNTR = 4, DO _fxs_cb UNTIL LCE;
            R1 = R0 AND R6;
            R0 = LSHIFT R0 BY R5;
            R1 = MIN(R1, R4);
            F1 = FLOAT R1;
            F1 = F1 * F7;
            F1 = F1 * F1;
_fxs_cb:    DM(I0, M6) = R1;
_fxs_cw: NOP;
