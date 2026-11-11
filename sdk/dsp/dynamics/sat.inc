// digikit DSP kernels: cubic soft clip.
//
// DK_SAT_STEP(OUT, IN1, ONE, HALF, K15, TMP)
//   maths:         c = clip(x, -1, 1); y = c (1.5 - 0.5 c c); the curve is linear-ish near 0 (slope 1.5), reaches
//                  +-1 with slope 0 at c = +-1, and is hard-clipped beyond |x| = 1
//   arguments:     OUT = y, IN1 = x, TMP = temporary, ONE = 1.0 (0x3f800000), HALF = 0.5 (0x3f000000),
//                  K15 = 1.5 (0x3fc00000); the caller loads the three constants before the loop (all F registers)
//   instructions:  5 (clip, c c, 0.5 c c, 1.5 - ..., c (...))
//   accuracy:      float32 roundings in this order: t = c c, t = t 0.5 (exact), t = 1.5 - t, y = c t; |y| <= 1
//
// The caller writes the trailing ";". CLIP uses |ONE| as the bound.

#define DK_SAT_STEP(OUT, IN1, ONE, HALF, K15, TMP) \
    OUT = CLIP IN1 BY ONE; \
    TMP = OUT * OUT; \
    TMP = TMP * HALF; \
    TMP = K15 - TMP; \
    OUT = OUT * TMP
