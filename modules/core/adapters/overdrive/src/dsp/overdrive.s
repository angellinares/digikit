// core/overdrive: flavours for the tracks' overdrive (the FX page's OVR TYPE, the stock OVR Routing with a wider range).
//
// The stock overdrive (FUN_1cb3d8, shared with the master) is: u = drive x, y = clamp(u, -1, 1), then three times
// y = y (2 - |y|) (a small-signal gain of 8), out = x + mix (y - x); drive and mix ramp across the block from the record.
// OVR TYPE's word is flavour << 9 | post << 8: 0 and 0x100 are the stock values (flavour 0, the stock curve).
//
// The frame split, after each of the two copies of the frame in FUN_1c2b24 (_od_split after the conditional one, _od_split2
// after the one each block makes near its end): for each track, keeps the flavour (bits 9..15 of its OVR TYPE halfword) in
// _od_flav and leaves only the routing bit in the frame copy, so the stock unpack sees pre / post. The frame copy is byte
// addressed, two bytes per halfword (split.inc). Only R0 and R1 change, and they are put back; the flags are dead at both sites.
//
// _od_sat (the two per-track calls of the overdrive, retargeted; the master's stay stock): the track's voice is
// 32 - R14 (the chain driver's countdown over the 32 voices). A block that is not 32 samples continues in the stock routine.
// Otherwise the stock routine's record handling and ramps, with the drive target boosted by 1 + d^2 / 2 (1.5 x at full),
// and the flavour's curve: 0 the stock curve on drive x, the others on z = 8 drive x:
//   1 TNH4  tanh(4 z)                  (a (7, 6) Pade approximant, clamped at |a| = 4.97)
//   2 HARD  clamp(z, -1, 1)
//   3 SOFT  z / (1 + |z|)
//   4 TUBE  tanh(z + 0.4) - tanh(0.4)  (asymmetric: even harmonics)
//   5 FOLD  a triangle fold of z
//   6 FUZZ  clamp(4 z + 0.3, -0.6, 1) - 0.3
//   7 SQR   clamp(64 z, -1, 1)
// C calling convention (R4 record, R8 in, R12 out, n at DM(1, I6)); saves what it uses beyond R0-R2, R4, R8, R12, I4. Only
// I3, I4 and I5 are used, the index registers the stock routine uses (their L registers are 0 in this context).

#include "consts.inc"

.SECTION/PM seg_pmco;
.GLOBAL _od_split;
.GLOBAL _od_split2;
.GLOBAL _od_sat;
.EXTERN _fw_frame;
.EXTERN _fw_sat;
.EXTERN _od_tail;
.EXTERN _od_tail2;

_od_split:
    CALL _od_do;
    JUMP _od_tail;
_od_split2:
    CALL _od_do;
    JUMP _od_tail2;
_od_do:
    DM(_od_save) = R0;
    DM(_od_save + 4) = R1;
#include "split.inc"
    R1 = DM(_od_save + 4);
    R0 = DM(_od_save);
    RTS;

_od_sat:
    R0 = DM(1, I6);
    R1 = 32;
    COMP(R0, R1);
    IF NE JUMP _fw_sat;
    R0 = 32;
    R0 = R0 - R14;                        // the voice
    R0 = FEXT R0 BY 1:4;                  // the track
    R0 = LSHIFT R0 BY 2;
    R1 = _od_flav;
    R0 = R0 + R1;
    I4 = R0;
    R0 = DM(0, I4);

    MODIFY(I7, -14) (NW);
    DM(-2, I6) = R3;
    DM(-3, I6) = R5;
    DM(-4, I6) = R9;
    DM(-5, I6) = R10;
    DM(-6, I6) = R11;
    DM(-7, I6) = R13;
    DM(-8, I6) = R14;
    DM(-9, I6) = R15;
    R1 = I3;
    DM(-10, I6) = R1;
    R1 = I5;
    DM(-11, I6) = R1;
    DM(-12, I6) = R7;
    R5 = PASS R0;                         // the flavour
    I5 = R8;
    I3 = R12;
    I4 = R4;

    // the stock routine's targets and ramps: drive -> min(max(rec 0, rec 1), 1), mix -> clamp(rec 2, 0, 1); then the boost
    R2 = DM(0, I4);
    R1 = DM(1, I4);
    COMP(F2, F1);
    IF LT F2 = PASS F1;
    R10 = 0x3f800000;
    F2 = MIN(F2, F10);
    R3 = 0x3f000000;
    F1 = F2 * F2;
    F1 = F1 * F3;
    F1 = F1 + F10;
    F2 = F2 * F1;
    R1 = DM(2, I4);
    R3 = 0;
    F1 = MAX(F1, F3);
    F1 = MIN(F1, F10);
    R13 = DM(5, I4);                      // drive now
    R12 = DM(12, I4);                     // mix now
    DM(4, I4) = R13;
    DM(5, I4) = R2;
    DM(11, I4) = R12;
    DM(12, I4) = R1;
    R3 = 0x3d000000;                      // 1 / 32
    F2 = F2 - F13;
    F8 = F2 * F3;                         // drive step
    F1 = F1 - F12;
    F11 = F1 * F3;                        // mix step
    DM(6, I4) = R8;
    DM(13, I4) = R11;

    R0 = _od_k;
    I4 = R0;
    R9 = DM(K_EIGHT, I4);
    R10 = DM(K_ONE, I4);
    R15 = DM(K_M1, I4);
    R5 = PASS R5;
    IF EQ JUMP _od_stk;
    R1 = 1;
    COMP(R5, R1);
    IF EQ JUMP _od_tnh4;
    R1 = 2;
    COMP(R5, R1);
    IF EQ JUMP _od_hard;
    R1 = 3;
    COMP(R5, R1);
    IF EQ JUMP _od_soft;
    R1 = 4;
    COMP(R5, R1);
    IF EQ JUMP _od_tube;
    R1 = 5;
    COMP(R5, R1);
    IF EQ JUMP _od_fold;
    R1 = 6;
    COMP(R5, R1);
    IF EQ JUMP _od_fuzz;

// SQR
    R3 = DM(K_SQR, I4);
    LCNTR = 32, DO _od_sqr_e UNTIL LCE;
    R14 = DM(I5, M6);
    F0 = F13 * F14;
    F0 = F0 * F9;
    F0 = F0 * F3;
    F0 = MIN(F0, F10);
    F0 = MAX(F0, F15);
    F0 = F0 - F14;
    F0 = F0 * F12;
    F0 = F0 + F14;
    F13 = F13 + F8;
    F12 = F12 + F11;
_od_sqr_e: DM(I3, M6) = R0;
    JUMP _od_ret;

_od_stk:
    R3 = DM(K_TWO, I4);
    LCNTR = 32, DO _od_stk_e UNTIL LCE;
    R14 = DM(I5, M6);
    F0 = F13 * F14;
    F0 = MIN(F0, F10);
    F0 = MAX(F0, F15);
    F1 = ABS F0;
    F1 = F3 - F1;
    F0 = F0 * F1;
    F1 = ABS F0;
    F1 = F3 - F1;
    F0 = F0 * F1;
    F1 = ABS F0;
    F1 = F3 - F1;
    F0 = F0 * F1;
    F0 = F0 - F14;
    F0 = F0 * F12;
    F0 = F0 + F14;
    F13 = F13 + F8;
    F12 = F12 + F11;
_od_stk_e: DM(I3, M6) = R0;
    JUMP _od_ret;

_od_hard:
    LCNTR = 32, DO _od_hard_e UNTIL LCE;
    R14 = DM(I5, M6);
    F0 = F13 * F14;
    F0 = F0 * F9;
    F0 = MIN(F0, F10);
    F0 = MAX(F0, F15);
    F0 = F0 - F14;
    F0 = F0 * F12;
    F0 = F0 + F14;
    F13 = F13 + F8;
    F12 = F12 + F11;
_od_hard_e: DM(I3, M6) = R0;
    JUMP _od_ret;

_od_fuzz:
    R3 = DM(K_FOUR, I4);
    R5 = DM(K_FUZZB, I4);
    R1 = DM(K_FUZZLO, I4);
    LCNTR = 32, DO _od_fuzz_e UNTIL LCE;
    R14 = DM(I5, M6);
    F0 = F13 * F14;
    F0 = F0 * F9;
    F0 = F0 * F3;
    F0 = F0 + F5;
    F0 = MIN(F0, F10);
    F0 = MAX(F0, F1);
    F0 = F0 - F5;
    F0 = F0 - F14;
    F0 = F0 * F12;
    F0 = F0 + F14;
    F13 = F13 + F8;
    F12 = F12 + F11;
_od_fuzz_e: DM(I3, M6) = R0;
    JUMP _od_ret;

_od_soft:
    R3 = DM(K_TWO, I4);
    LCNTR = 32, DO _od_soft_e UNTIL LCE;
    R14 = DM(I5, M6);
    F0 = F13 * F14;
    F0 = F0 * F9;
    F1 = ABS F0;
    F1 = F1 + F10;                        // d = 1 + |z|
    F2 = RECIPS F1;
    F4 = F1 * F2;
    F4 = F3 - F4;
    F2 = F2 * F4;
    F4 = F1 * F2;
    F4 = F3 - F4;
    F2 = F2 * F4;                         // 1 / d
    F0 = F0 * F2;
    F0 = F0 - F14;
    F0 = F0 * F12;
    F0 = F0 + F14;
    F13 = F13 + F8;
    F12 = F12 + F11;
_od_soft_e: DM(I3, M6) = R0;
    JUMP _od_ret;

_od_fold:
    R3 = DM(K_QUARTER, I4);
    R5 = DM(K_K64, I4);
    LCNTR = 32, DO _od_fold_e UNTIL LCE;
    R14 = DM(I5, M6);
    F0 = F13 * F14;
    F0 = F0 * F9;
    F0 = F0 + F10;
    F0 = F0 * F3;
    F0 = F0 + F5;                         // t = (z + 1) / 4 + 64 > 0
    R1 = TRUNC F0;
    F1 = FLOAT R1;
    F0 = F0 - F1;                         // frac(t)
    R1 = DM(K_HALF, I4);
    F0 = F0 - F1;
    F0 = ABS F0;
    R1 = DM(K_FOUR, I4);
    F0 = F0 * F1;
    F0 = F10 - F0;                        // 1 - 4 |frac - 1/2|
    F0 = F0 - F14;
    F0 = F0 * F12;
    F0 = F0 + F14;
    F13 = F13 + F8;
    F12 = F12 + F11;
_od_fold_e: DM(I3, M6) = R0;
    JUMP _od_ret;

_od_tnh4:
    R3 = DM(K_FOUR, I4);
    R5 = 0;
    R7 = 0;
    JUMP _od_tanhs;
_od_tube:
    R3 = DM(K_ONE, I4);
    R5 = DM(K_TUBEB, I4);
    R7 = DM(K_TUBEO, I4);
_od_tanhs:
    // y = tanh(F3 z + F5) - F7: TNH4 F3 = 4, F5 = F7 = 0; TUBE F3 = 1, F5 = 0.4, F7 = tanh(0.4)
    LCNTR = 32, DO _od_tanh_e UNTIL LCE;
    R14 = DM(I5, M6);
    F0 = F13 * F14;
    F0 = F0 * F9;
    F0 = F0 * F3;
    F0 = F0 + F5;
    CALL _od_tanh;
    F0 = F0 - F7;
    F0 = F0 - F14;
    F0 = F0 * F12;
    F0 = F0 + F14;
    F13 = F13 + F8;
    F12 = F12 + F11;
_od_tanh_e: DM(I3, M6) = R0;
    JUMP _od_ret;

_od_ret:
    R3 = DM(-2, I6);
    R5 = DM(-3, I6);
    R9 = DM(-4, I6);
    R10 = DM(-5, I6);
    R11 = DM(-6, I6);
    R13 = DM(-7, I6);
    R14 = DM(-8, I6);
    R15 = DM(-9, I6);
    R1 = DM(-10, I6);
    I3 = R1;
    R1 = DM(-11, I6);
    I5 = R1;
    R7 = DM(-12, I6);
    I12 = DM(-1, I6);
    JUMP (M14, I12) (DB);
    RFRAME;
    NOP;

// F0 = tanh(F0) by the (7, 6) Pade approximant a (135135 + 17325 a^2 + 378 a^4 + a^6) / (135135 + 62370 a^2 + 3150 a^4 +
// 28 a^6), a clamped to +-4.97; clobbers F1, F2, F4 (F6 and F7 are kept)
_od_tanh:
    DM(_od_save + 8) = R6;
    DM(_od_save + 12) = R7;
    R1 = DM(K_TCLAMP, I4);
    F0 = MIN(F0, F1);
    F1 = -F1;
    F0 = MAX(F0, F1);
    F1 = F0 * F0;                         // a^2
    R2 = DM(K_P3, I4);                    // 1
    R4 = DM(K_P2, I4);
    F2 = F2 * F1;
    F2 = F2 + F4;                         // a^2 + 378
    R4 = DM(K_P1, I4);
    F2 = F2 * F1;
    F2 = F2 + F4;
    R4 = DM(K_P0, I4);
    F2 = F2 * F1;
    F2 = F2 + F4;
    F2 = F2 * F0;                         // numerator
    R6 = DM(K_Q3, I4);
    R4 = DM(K_Q2, I4);
    F6 = F6 * F1;
    F6 = F6 + F4;
    R4 = DM(K_Q1, I4);
    F6 = F6 * F1;
    F6 = F6 + F4;
    R4 = DM(K_P0, I4);
    F6 = F6 * F1;
    F6 = F6 + F4;                         // denominator
    F7 = RECIPS F6;
    R4 = DM(K_TWO, I4);
    F1 = F6 * F7;
    F1 = F4 - F1;
    F7 = F7 * F1;
    F1 = F6 * F7;
    F1 = F4 - F1;
    F7 = F7 * F1;
    F0 = F2 * F7;
    R7 = DM(_od_save + 12);
    R6 = DM(_od_save + 8);
    RTS;

.SECTION/DM seg_dmda;
.PKDATA _od_flav, 64;                     // the flavour of each track, from its last frame
.PKDATA _od_save, 16;
// _od_k: the curve constants, at K_<name> (consts.inc)
#include "k.inc"
