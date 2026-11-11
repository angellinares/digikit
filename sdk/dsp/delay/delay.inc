// digikit DSP kernels: delay line block routine (linearly interpolated read on a hardware circular buffer).
//
// _dk_delay (C: dk_delay.)   void dk_delay(int x, int y, int st)   convention: dsp/contract/call.inc
//   maths:         ring of L words, write offset w, delay d in samples (a float, constant over the block, 1.0 <= d <= L - 2).
//                  k = trunc(d), f = d - k, g = 1 - f. Per sample, in this order:
//                    ring[w] = x;  a = ring[w - k];  b = ring[w - k - 1];  y = g a + f b;  w = w + 1      (indices mod L)
//                  so d = 1.0 gives the previous input, and d = 2.5 the mean of the inputs 2 and 3 samples back.
//                  Operation order (one float32 rounding each): p = g a; q = f b; y = p + q. The reference model is dsp_ref.delay.
//                  k and f are computed once per call. The conversion is TRUNC (round toward zero, whatever MODE1 says), which is
//                  the floor for d >= 1. f = d - float(k) is exact. d outside [1, L - 2] is not checked.
//   arguments:     R4 = input block, R8 = output block (may equal R4), R12 = state block; all normal-word DM addresses
//                  state block, 4 words: ring base (address), ring length L (words, >= 34), write offset w (0 <= w < L, written
//                  back at exit), delay d (float32 bits)
//   block:         DK_BLOCK (32) samples. The routine reads only x[0] .. x[31] and the ring (circular buffers keep every ring
//                  access inside it). The loop reads x[n] before it writes y[n], so in place (R8 = R4) is safe. The ring must
//                  not overlap the input or output block.
//   registers:     F0 x / q, F1 and F2 a and b (swapped every sample: the loop is unrolled by 2), F4 f, F8 g, F5 p / y,
//                  I4 input pointer, I0 output pointer, I1 write pointer and I2 read pointer (both circular: B = ring base,
//                  L = ring length, post-modify M6 = 1), M4 = -(k + 1) for the setup only, M6 = 1.
//   saves:         R5, I0, I1, I2 (4 slots, S = 4; M4 is not callee-saved). B1 and B2 are written and not restored (with L = 0 they have no effect;
//                  the callee-saved list of call.inc holds B3 only). L1 and L2 are 0 on return, as on entry: the routine sets
//                  them to the ring length and puts them back to 0 on its one exit.
//   instructions:  4 per sample in the loop (4 memory accesses on one bus: x load, ring store, ring load, y store; the multiplies
//                  and the add ride in those instructions). Per call 172 in the routine: 30 before the loop, 128 in it
//                  (8 x 16 pairs), 14 after; 175 with DK_CALL. Average 5.5 per sample with the call. Measured by the test [D].
//   loop schedule: one sample, 4 instructions, no software pipelining across samples; a(n+1) is loaded after the ring store of
//                  sample n, so d = 1.0 (a(n+1) = x[n]) is correct. b(n+1) = a(n) stays in its register (the register names swap).
//                    slot 0: p = g a;  load x
//                    slot 1: q = f b;  ring[w] = x, w += 1
//                    slot 2: y = p + q;  load a(n+1) from the read pointer, read pointer += 1
//                    slot 3: store y
//                  Setup: the read pointer starts at w, moves back k + 1 cells with a dummy post-modify load (M4 = -(k + 1); the register-modifier MODIFY has no form in our
//                  decoder tables),
//                  and two post-modify loads fetch b(0) = ring[w - k - 1] and a(0) = ring[w - k].
//   [O]            Circular buffers with B/L/post-modify, a dummy load as the circular modify, the multiplier with a data move in one
//                  instruction and the symbol `dk_delay.` are written from the manuals and findings; the test is the first assembly.
//   Manual:        Data Address Generators, "Circular Buffering Mode"; Processing Elements, "Multifunction Computations".
//
// Needs MODE1.CBUFEN set (circular wrap of load/store post-modify; firmware start-up sets it and the module contract keeps it).
// Needs "dsp/contract/call.inc" and DK_BLOCK (dk_machine.inc or dk_fx.inc) included before this file.

.SECTION/PM seg_pmco;
.GLOBAL _dk_delay;
.GLOBAL dk_delay.;

_dk_delay:
dk_delay.:
    DK_ENTER(4);
    I4 = R12;                          // state pointer
    DK_SAVE(2, I0);
    DK_SAVE(3, I1);
    DK_SAVE(4, I2);
    DK_SAVE(5, R5);
    I0 = R8;                           // output pointer (R8 is free now)
    R1 = DM(0, I4);                    // ring base
    R2 = DM(1, I4);                    // ring length L
    R0 = DM(2, I4);                    // write offset w
    F8 = DM(3, I4);                    // d
    I4 = R4;                           // input pointer
    R4 = TRUNC F8;                     // k = floor(d)
    R5 = R0 + R1;                      // ring base + w
    B2 = R1;
    L2 = R2;
    I2 = R5;
    R0 = NOT R4;                       // -(k + 1)
    M4 = R0;
    B1 = R1;
    L1 = R2;
    I1 = R5;
    F5 = FLOAT R4;                     // float(k)
    F4 = F8 - F5;                      // f = d - k
    R5 = 0x3f800000;                   // 1.0
    F8 = F5 - F4;                      // g = 1 - f
    F2 = DM(I2, M4);                   // dummy load (reads ring[w]): the read pointer moves to w - k - 1, circular
    F2 = DM(I2, M6);                   // b(0) = ring[w - k - 1]
    F1 = DM(I2, M6);                   // a(0) = ring[w - k]
    LCNTR = 16, DO _dk_delay_end UNTIL LCE;                  // DK_BLOCK / 2 pairs of samples
        F5 = F8 * F1, F0 = DM(I4, M6);                       // even sample, slot 0: p = g a; load x
        F0 = F4 * F2, DM(I1, M6) = F0;                       // slot 1: q = f b; ring[w] = x
        F5 = F5 + F0, F2 = DM(I2, M6);                       // slot 2: y = p + q; load a(n+1) into the b register
        DM(I0, M6) = F5;                                     // slot 3: store y
        F5 = F8 * F2, F0 = DM(I4, M6);                       // odd sample, slot 0: a is in F2, b in F1
        F0 = F4 * F1, DM(I1, M6) = F0;                       // slot 1
        F5 = F5 + F0, F1 = DM(I2, M6);                       // slot 2: load a(n+1) into F1
_dk_delay_end: DM(I0, M6) = F5;                              // slot 3
    I4 = R12;
    R1 = I1;                           // ring base + w after the block
    L1 = 0;
    L2 = 0;
    DK_RETADDR;
    R2 = DM(0, I4);                    // ring base
    R0 = R1 - R2;                      // w after the block
    DM(2, I4) = R0;
    DK_RESTORE(I0, 2);
    DK_RESTORE(I1, 3);
    DK_RESTORE(I2, 4);
    DK_LEAVE(R5, 5);
