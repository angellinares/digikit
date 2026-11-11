// digikit DSP kernels: 2x decimator block routine (23-tap half-band FIR, circular history).
//
// _dk_halfband_down (C: dk_halfband_down.)   void dk_halfband_down(int x, int y, int st)   convention: dsp/contract/call.inc
//   maths:         input stream x[t] (t counts over all calls; a call takes 2 DK_BLOCK = 64 inputs), output y[m] = one per two
//                  inputs, m = 0 .. 31 in a call, with x[2m] and x[2m + 1] the two newest inputs for output m:
//                    y[m] = 0.5 x[2m - 11] + sum over j in {1, 3, 5, 7, 9, 11} of c_j (x[2m - 11 - j] + x[2m - 11 + j])
//                  This is the 23-tap filter with h[0] = 0.5, h[+-j] = c_j for odd j and h[+-j] = 0 for even j, centred on
//                  x[2m - 11]: group delay 11 input samples. The inputs used are x[2m - 22] .. x[2m]. Indices before the call
//                  (t < 0) come from the history. The table _dk_halfband_c1 .. c11 below holds d_j = 2 c_j (exact doubling,
//                  dsp_ref.HALFBAND_C; a Kaiser-window half-band, DC gain 1.0).
//                  Operation order (one float32 rounding each; the factor 2 is folded in so that no 0.5 register is needed
//                  and the centre tap is the ALU average): s1 = x[2m-10] + x[2m-12]; s3 = x[2m-8] + x[2m-14];
//                  s5 = x[2m-6] + x[2m-16]; s7 = x[2m-4] + x[2m-18]; s9 = x[2m-2] + x[2m-20]; s11 = x[2m] + x[2m-22];
//                  p_j = d_j s_j; t = ((((p1 + p3) + p5) + p7) + p11) + p9; y = (x[2m-11] + t) / 2 (the ALU average,
//                  one rounding). The reference model is dsp_ref.halfband_down.
//   arguments:     R4 = input block (64 floats), R8 = output block (32 floats), R12 = state block; normal-word DM addresses
//                  state block, 2 words: base address of the history ring (32 words), history offset o (0 <= o < 32)
//                  Ring: cell o is where the next input is written; the cells o - 1, o - 2, .. o - 22 (mod 32) hold the last
//                  inputs, newest first. The filter needs 22 past inputs plus the 2 new ones = 24 cells; the ring has 32 cells
//                  because 64 inputs are then exactly 2 turns, so the offset o is the same after every call and the routine
//                  does not write the state block (a 24-cell ring would need the new offset written back and a second
//                  modifier for the pointer walk). A zeroed ring is a silent start. After a call the ring holds the last
//                  inputs in the same cells. The routine reads only x[0] .. x[63] and the ring; it writes the ring and
//                  y[0] .. y[31].
//   block:         DK_BLOCK (32) outputs, 64 inputs. The input and output blocks must not overlap the ring.
//   registers:     coefficients F4 d1, F5 d3, F6 d5, F1 d7, F3 d9, F2 d11 (multiplier operands F0-F3 and F4-F7); sums in F0
//                  (s1, s3, s5) and F7 (s7, s9, s11); data F8-F15 (ALU operands F8-F11 and F12-F15): F15 x[2m] (next: x[2m+2]
//                  is loaded into it while s11 is formed), F13 x[2m-11], the rest are short-lived history values and products.
//                  I4 input pointer, I0 output pointer, I1 history pointer (circular, B1 = ring base, L1 = 32),
//                  M1 = -2, M2 = -8, M4 = -3 (M4 is not callee-saved; the short store form takes M4-M7 only), M6 = 1, M7 = -1. I0 holds the coefficient table pointer in the setup.
//   saves:         R3, R5, R6, R7, R9, R10, R11, R13, R14, R15, I0, I1, M1, M2 (14 slots, S = 14). B1 is written and not
//                  restored (with L = 0 it has no effect; the callee-saved list of call.inc holds B3 only). L1 is 0 on return.
//   instructions:  20 per output in the loop. The 12 history reads, 2 input loads, 2 history writes and the output store are
//                  17 accesses on one data bus; the ALU does 12 operations (6 pre-adds, 5 sums, the average) starting at slot 7,
//                  so the last one is in slot 18 and the store in slot 19. Per call 698 in the routine: 39 before the loop,
//                  620 in the loop (20 x 31 outputs), 21 for the last output (20 and the LCNTR of its one-pass loop), 18 after; 701 with DK_CALL.
//                  Average 21.9 per output with the call. Measured by the test [D].
//                  [O] A shorter loop needs the window in registers (renamed registers over several outputs) or long-word
//                  accesses; there are only 16 data registers and 6 are coefficients, 2 are sums.
//   loop schedule: the history pointer I1 walks one fixed path per output, all with post-modify (circular buffers allow no
//                  pre-modify): reads o-2, o-4, o-6, o-8 (M1 = -2), o-10, o-11 (M7 = -1), o-12 .. o-20 (M1), o-22 (M2 = -8)
//                  leave it on o + 2, the cell of the next output's first write; the two writes (M6, then M4 = -3) leave it on
//                  o' - 2 again. The inputs of output m + 1 are loaded and written while the sums of output m finish, so the
//                  last output is a copy of the loop body without those four instructions.
//                    slot 0 .. 6   x[2m-2] x[2m-4] x[2m-6] x[2m-8] x[2m-10] centre x[2m-12] loaded (slot 4 into F0)
//                    slot 7  s1 = + x[2m-12]; load x[2m-14]          slot 8  s3; p1; load x[2m-16]
//                    slot 9  s5; p3; load x[2m-18]                   slot 10 p1 + p3; p5; load x[2m-20]
//                    slot 11 s7; load x[2m-22]                       slot 12 s11; p7; load x[2m+2]
//                    slot 13 + p5; p11; write x[2m+2]                slot 14 s9; load x[2m+3]
//                    slot 15 + p7; p9; write x[2m+3]                 slot 16 + p11
//                    slot 17 + p9                                    slot 18 y = (centre + t) / 2      slot 19 store y
//   [O]            Multifunction (multiplier, ALU and a memory access in one instruction) with the operand register files of the
//                  manual, the ALU average `Fn = (Fx + Fy) / 2`, circular buffers, `.SECTION/DM` data in an include, the symbols
//                  and the table address `I0 = _dk_halfband_c1` as an immediate are written from the manuals and findings; the
//                  test is the first assembly. The six `.VAR` words are assumed to be consecutive.
//   Manual:        Data Address Generators, "Circular Buffering Mode"; Processing Elements, "Multifunction Computations".
//
// Needs MODE1.CBUFEN set (circular wrap of load/store post-modify; firmware start-up sets it and the module contract keeps it).
// Needs "dsp/contract/call.inc" and DK_BLOCK (dk_machine.inc or dk_fx.inc) included before this file. Include it once per module.

.SECTION/PM seg_pmco;
.GLOBAL _dk_halfband_down;
.GLOBAL dk_halfband_down.;

_dk_halfband_down:
dk_halfband_down.:
    DK_ENTER(14);
    I4 = R12;                          // state pointer
    DK_SAVE(2, I1);
    DK_SAVE(3, I0);
    DK_SAVE(4, M1);
    DK_SAVE(5, M2);
    R0 = DM(0, I4);                    // ring base
    R1 = DM(1, I4);                    // history offset o
    I0 = _dk_halfband_c1;              // coefficient table pointer (I0 is the output pointer later)
    B1 = R0;
    L1 = 32;
    R2 = R0 + R1;                      // ring base + o
    I1 = R2;
    M1 = -2;
    M2 = -8;
    M4 = -3;
    I4 = R4;                           // input pointer
    DK_SAVE(6, R3);
    DK_SAVE(7, R5);
    DK_SAVE(8, R6);
    DK_SAVE(9, R7);
    DK_SAVE(10, R9);
    DK_SAVE(11, R10);
    DK_SAVE(12, R11);
    DK_SAVE(13, R13);
    DK_SAVE(14, R14);
    DK_SAVE(15, R15);
    F4 = DM(0, I0);                    // d1
    F5 = DM(1, I0);                    // d3
    F6 = DM(2, I0);                    // d5
    F1 = DM(3, I0);                    // d7
    F3 = DM(4, I0);                    // d9
    F2 = DM(5, I0);                    // d11
    F15 = DM(I4, M6);                  // prime: x[0]
    F14 = DM(I4, M6);                  // x[1]
    DM(I1, M6) = F15;                  // write x[0], x[1] to the ring; the history pointer ends on o - 2
    DM(I1, M4) = F14;
    I0 = R8;                           // output pointer (after the stores: a store right before LCNTR is listed with the wrong width)
    LCNTR = DK_BLOCK - 1, DO _dk_halfband_down_end UNTIL LCE;
        F10 = DM(I1, M1);                                         // slot 0: x[2m-2]
        F11 = DM(I1, M1);                                         // slot 1: x[2m-4]
        F9 = DM(I1, M1);                                          // slot 2: x[2m-6]
        F8 = DM(I1, M1);                                          // slot 3: x[2m-8]
        F0 = DM(I1, M7);                                          // slot 4: x[2m-10]
        F13 = DM(I1, M7);                                         // slot 5: centre x[2m-11]
        F14 = DM(I1, M1);                                         // slot 6: x[2m-12]
        F0 = F0 + F14, F12 = DM(I1, M1);                          // slot 7: s1; x[2m-14]
        F8 = F0 * F4, F0 = F8 + F12, F12 = DM(I1, M1);            // slot 8: p1; s3; x[2m-16]
        F12 = F0 * F5, F0 = F9 + F12, F9 = DM(I1, M1);            // slot 9: p3; s5; x[2m-18]
        F12 = F0 * F6, F8 = F8 + F12, F14 = DM(I1, M1);           // slot 10: p5; p1 + p3; x[2m-20]
        F7 = F11 + F9, F11 = DM(I1, M2);                          // slot 11: s7; x[2m-22]
        F9 = F1 * F7, F7 = F11 + F15, F15 = DM(I4, M6);           // slot 12: p7; s11; load x[2m+2]
        F11 = F2 * F7, F12 = F8 + F12, DM(I1, M6) = F15;          // slot 13: p11; + p5; write x[2m+2]
        F7 = F10 + F14, F14 = DM(I4, M6);                         // slot 14: s9; load x[2m+3]
        F9 = F3 * F7, F8 = F9 + F12, DM(I1, M4) = F14;            // slot 15: p9; + p7; write x[2m+3]
        F10 = F8 + F11;                                           // slot 16: + p11
        F12 = F10 + F9;                                           // slot 17: + p9 = t
        F8 = (F13 + F12) / 2;                                     // slot 18: y = (centre + t) / 2
_dk_halfband_down_end: DM(I0, M6) = F8;                           // slot 19: store y
    // output DK_BLOCK - 1: as the loop body, without the loads and writes of the next inputs; a loop of one pass, because outside a
    // DO loop Selache lists the short forms with M1 and M2 by a width that our decoder does not share
    LCNTR = 1, DO _dk_halfband_down_last UNTIL LCE;
        F10 = DM(I1, M1);
        F11 = DM(I1, M1);
        F9 = DM(I1, M1);
        F8 = DM(I1, M1);
        F0 = DM(I1, M7);
        F13 = DM(I1, M7);
        F14 = DM(I1, M1);
        F0 = F0 + F14, F12 = DM(I1, M1);
        F8 = F0 * F4, F0 = F8 + F12, F12 = DM(I1, M1);
        F12 = F0 * F5, F0 = F9 + F12, F9 = DM(I1, M1);
        F12 = F0 * F6, F8 = F8 + F12, F14 = DM(I1, M1);
        F7 = F11 + F9, F11 = DM(I1, M2);
        F9 = F1 * F7, F7 = F11 + F15;
        F11 = F2 * F7, F12 = F8 + F12;
        F7 = F10 + F14;
        F9 = F3 * F7, F8 = F9 + F12;
        F10 = F8 + F11;
        F12 = F10 + F9;
        F8 = (F13 + F12) / 2;
_dk_halfband_down_last: DM(I0, M6) = F8;
    L1 = 0;
    DK_RETADDR;
    DK_RESTORE(R3, 6);
    DK_RESTORE(R5, 7);
    DK_RESTORE(R6, 8);
    DK_RESTORE(R7, 9);
    DK_RESTORE(R9, 10);
    DK_RESTORE(R10, 11);
    DK_RESTORE(R11, 12);
    DK_RESTORE(R13, 13);
    DK_RESTORE(R14, 14);
    DK_RESTORE(I0, 3);
    DK_RESTORE(M1, 4);
    DK_RESTORE(M2, 5);
    DK_RESTORE(I1, 2);
    DK_LEAVE(R15, 15);

// Coefficients: d_j = 2 c_j for the odd taps of the 23-tap half-band (dsp_ref.HALFBAND_C, float32 words).
.SECTION/DM seg_dmda;
.GLOBAL _dk_halfband_c1;
.VAR _dk_halfband_c1 = 0x3f219ddc;  // d1 = 2 c1 = 0.631314993 (c1 = 0.31565751)
.VAR _dk_halfband_c3 = 0xbe43ddd1;  // d3 = 2 c3 = -0.19127585 (c3 = -0.095637926)
.VAR _dk_halfband_c5 = 0x3dc09d75;  // d5 = 2 c5 = 0.0940503255 (c5 = 0.0470251621)
.VAR _dk_halfband_c7 = 0xbd473c9a;  // d7 = 2 c7 = -0.0486417785 (c7 = -0.0243208889)
.VAR _dk_halfband_c9 = 0x3cbd53e0;  // d9 = 2 c9 = 0.0231112838 (c9 = 0.0115556416)
.VAR _dk_halfband_c11 = 0xbc0c3b0a;  // d11 = 2 c11 = -0.00855899788 (c11 = -0.00427949876)
