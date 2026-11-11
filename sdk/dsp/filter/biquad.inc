// digikit DSP kernels: biquad block routine (second-order section, transposed direct form II, a0 = 1).
//
// _dk_biquad (C: dk_biquad.)   void dk_biquad(int x, int y, int st)   convention: dsp/contract/call.inc
//   maths:         per sample, with the state (s1, s2) carried between samples and between blocks:
//                    y  = b0 x + s1
//                    s1 = (b1 x + s2) - a1 y
//                    s2 = b2 x - a2 y
//                  Operation order (one float32 rounding each): p0 = b0 x; p1 = b1 x; p2 = b2 x; y = p0 + s1; u = s2 + p1;
//                  q = a1 y; s1 = u - q; q = a2 y; s2 = p2 - q. The reference model is dsp_ref.biquad.
//   arguments:     R4 = input block, R8 = output block (may equal R4), R12 = state block; all normal-word DM addresses
//                  state block, 7 floats: b0, b1, b2, a1, a2, s1, s2 (s1 and s2 are read at entry and written back at exit)
//   block:         DK_BLOCK (32) samples. The routine reads only x[0] .. x[31]. The loop writes y[n] before it reads
//                  x[n + 2], so in place (R8 = R4) is safe.
//   registers:     F0 x (next), F1 a1, F2 a2, F4 y, F5 b0, F6 b1, F7 b2, F8 s2 / u, F9 p0, F10 p2, F13 p1, F14 q, F15 s1,
//                  I4 input pointer, I0 output pointer, M6 = 1. Multiplier operands are in F0-F3 and F4-F7, ALU operands
//                  in F8-F11 and F12-F15, as the multifunction form needs. The routine keeps R12 unchanged.
//   saves:         R5, R6, R7, R9, R10, R13, R14, R15, I0 (9 slots, S = 9). Nothing else callee-saved is written.
//   instructions:  5 per sample in the loop (5 multiplies on one multiplier, so 5 is the floor for this form). Per call
//                  200 in the routine: 25 before the loop, 160 in the loop and the two samples after it (5 x 30 + 5 + 5),
//                  15 after; 203 with DK_CALL.
//                  Average 6.3 per sample with the call. Counted by reading, [O] until the test measures it.
//   loop schedule: one iteration handles sample n and starts the products of sample n + 1 (software pipelined).
//                    slot 0: p0' = b0 x[n+1];  y = p0 + s1
//                    slot 1: p1' = b1 x[n+1];  u = s2 + p1;  store y[n]
//                    slot 2: q = a1 y
//                    slot 3: q = a2 y;         s1 = u - q (the a1 product)
//                    slot 4: p2' = b2 x[n+1];  s2 = p2 - q (the a2 product);  load x[n+2]
//                  The chain y -> q -> s1 -> y' is 3 instructions of the 5. No DAG register is used within 4 instructions
//                  of its load; no multiplier input comes from the previous instruction's result.
//   [O]            Multifunction (multiplier, ALU and a memory access in one instruction), the symbol `dk_biquad.` and the
//                  CJUMP frame are written from the manuals and findings; the test is the first assembly.
//   Manual:        Processing Elements, "Multifunction Computations", "Software Pipelining for Multifunction Instructions".
//
// Needs "dsp/contract/call.inc" and DK_BLOCK (dk_machine.inc or dk_fx.inc) included before this file.

.SECTION/PM seg_pmco;
.GLOBAL _dk_biquad;
.GLOBAL dk_biquad.;

_dk_biquad:
dk_biquad.:
    DK_ENTER(9);
    I4 = R12;                          // state pointer
    DK_SAVE(2, R5);
    DK_SAVE(3, R6);
    DK_SAVE(4, R7);
    DK_SAVE(5, R15);
    DK_SAVE(6, I0);
    I0 = R8;                           // output pointer
    F5 = DM(0, I4);                    // b0
    F6 = DM(1, I4);                    // b1
    F7 = DM(2, I4);                    // b2
    F1 = DM(3, I4);                    // a1
    F2 = DM(4, I4);                    // a2
    F15 = DM(5, I4);                   // s1
    F8 = DM(6, I4);                    // s2 (R8 is free now)
    I4 = R4;                           // input pointer
    DK_SAVE(7, R9);
    DK_SAVE(8, R10);
    DK_SAVE(9, R13);
    DK_SAVE(10, R14);
    F0 = DM(I4, M6);                   // prime: x[0]
    F9 = F0 * F5;                      // p0 of x[0]
    F13 = F0 * F6;                     // p1 of x[0]
    F10 = F0 * F7, F0 = DM(I4, M6);    // p2 of x[0]; load x[1]
    LCNTR = DK_BLOCK - 2, DO _dk_biquad_end UNTIL LCE;
        F9 = F0 * F5, F4 = F9 + F15;                         // slot 0: p0 of x[n+1]; y = p0 + s1
        F13 = F0 * F6, F8 = F8 + F13, DM(I0, M6) = F4;       // slot 1: p1 of x[n+1]; u = s2 + p1; store y[n]
        F14 = F1 * F4;                                       // slot 2: q = a1 y
        F14 = F2 * F4, F15 = F8 - F14;                       // slot 3: q = a2 y; s1 = u - q
_dk_biquad_end: F10 = F0 * F7, F8 = F10 - F14, F0 = DM(I4, M6);  // slot 4: p2 of x[n+1]; s2 = p2 - q; load x[n+2]
    // sample DK_BLOCK - 2: as the loop body, without the load (x[DK_BLOCK - 1] is already in F0)
    F9 = F0 * F5, F4 = F9 + F15;
    F13 = F0 * F6, F8 = F8 + F13, DM(I0, M6) = F4;
    F14 = F1 * F4;
    F14 = F2 * F4, F15 = F8 - F14;
    F10 = F0 * F7, F8 = F10 - F14;
    // sample DK_BLOCK - 1: no products for a next sample
    F4 = F9 + F15;
    F8 = F8 + F13, DM(I0, M6) = F4;
    F14 = F1 * F4;
    F14 = F2 * F4, F15 = F8 - F14;
    F8 = F10 - F14;
    I4 = R12;
    DK_RETADDR;
    DK_RESTORE(R5, 2);
    DK_RESTORE(R6, 3);
    DK_RESTORE(R7, 4);
    DM(5, I4) = F15;                   // s1 back
    DM(6, I4) = F8;                    // s2 back
    DK_RESTORE(I0, 6);
    DK_RESTORE(R9, 7);
    DK_RESTORE(R10, 8);
    DK_RESTORE(R13, 9);
    DK_RESTORE(R14, 10);
    DK_LEAVE(R15, 5);
