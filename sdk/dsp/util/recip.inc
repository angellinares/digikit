// digikit DSP kernels: reciprocal, divide and reciprocal square root (hardware seed, Newton steps).
//
// The core gives a seed (SHARC+ PRM, Processing Elements, "Reciprocal/Square Root Instructions"): RECIPS is accurate to 8
// bits, RSQRTS to 4 bits. A Newton step squares the relative error e (and adds rounding), so:
//   reciprocal:  e' = e^2, 2^-8 -> 2^-16 -> 2^-32: 2 steps reach float32 (2^-24); the stock divide uses 3 only to also
//                round the last bit and guard the range.
//   rsqrt:       e' = 1.5 e^2 (+ 0.5 e^3), 2^-4 -> 6e-3 -> 5e-5 -> 4e-9: 3 steps; 2 steps leave 5e-5.
// Range: the seed is the special value for 0, NaN, and (RECIPS) |x| >= 2^126; RSQRTS of a negative number or NaN is all
// ones. These macros do not guard the range or the sign: the caller keeps x in a normal range (1e-30 to 1e30).
//
// DK_RECIP(OUT, IN1, TWO, TMP)
//   maths:         y = RECIPS x; twice: y = y (2 - x y)
//   arguments:     OUT = y (OUT must not be IN1), IN1 = x, TWO = 2.0 (0x40000000), TMP = temporary (all F registers)
//   instructions:  7 (seed, then 2 x 3)
//   accuracy:      relative error at most about 2^-21 (4 ulp bound; about 1.5 ulp typical); sign follows x
//
// DK_DIV(QUO, NUM, DEN, TWO, TMP, TMP2)
//   maths:         q = n (1 / d) with DK_RECIP
//   arguments:     QUO = q, NUM = n, DEN = d, TWO = 2.0, TMP and TMP2 = temporaries (TMP2 holds 1 / d; QUO may be NUM;
//                  QUO and TMP2 must not be DEN)
//   instructions:  8 (7 + the multiply)
//   accuracy:      relative error at most about 2^-20 (reciprocal error plus one rounding)
//
// DK_RSQRT(OUT, IN1, HALF, K15, TMP, TMP2)
//   maths:         y = RSQRTS x; three times: y = y (1.5 - 0.5 x y y)
//   arguments:     OUT = y (OUT must not be IN1), IN1 = x > 0, HALF = 0.5 (0x3f000000), K15 = 1.5 (0x3fc00000),
//                  TMP and TMP2 = temporaries (all F registers)
//   instructions:  16 (seed, then 3 x 5)
//   accuracy:      relative error at most about 2^-21 (4 ulp bound)
//
// The caller writes the trailing ";".

#define DK_RECIP(OUT, IN1, TWO, TMP) \
    OUT = RECIPS IN1; \
    TMP = IN1 * OUT; \
    TMP = TWO - TMP; \
    OUT = OUT * TMP; \
    TMP = IN1 * OUT; \
    TMP = TWO - TMP; \
    OUT = OUT * TMP

#define DK_DIV(QUO, NUM, DEN, TWO, TMP, TMP2) \
    DK_RECIP(TMP2, DEN, TWO, TMP); \
    QUO = NUM * TMP2

#define DK_RSQRT(OUT, IN1, HALF, K15, TMP, TMP2) \
    OUT = RSQRTS IN1; \
    TMP = IN1 * OUT; \
    TMP2 = TMP * OUT; \
    TMP2 = TMP2 * HALF; \
    TMP2 = K15 - TMP2; \
    OUT = OUT * TMP2; \
    TMP = IN1 * OUT; \
    TMP2 = TMP * OUT; \
    TMP2 = TMP2 * HALF; \
    TMP2 = K15 - TMP2; \
    OUT = OUT * TMP2; \
    TMP = IN1 * OUT; \
    TMP2 = TMP * OUT; \
    TMP2 = TMP2 * HALF; \
    TMP2 = K15 - TMP2; \
    OUT = OUT * TMP2
