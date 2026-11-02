// digikit DSP kernels: gain and mix, step macros for a module's own block loop.
//
// DK_GAIN_STEP(OUT, IN1, GN)
//   maths:         y = x g
//   arguments:     OUT = y, IN1 = x, GN = g (all F registers; OUT may be IN1)
//   instructions:  1 (multiplier)
//   accuracy:      one float32 rounding; equal to numpy float32 x * g
//
// DK_MIX_STEP(OUT, IA, IB, MIXV, TMP)
//   maths:         y = a + m (b - a); m = 0 gives a, m = 1 gives b to within rounding
//   arguments:     OUT = y, IA = a, IB = b, MIXV = m, TMP = temporary (all F registers; OUT may be IA; TMP is not an input)
//   instructions:  3 (subtract, multiply, add; a dependent chain)
//   accuracy:      three float32 roundings, in this order: t = b - a, t = t m, y = a + t
//
// The caller writes the trailing ";" and chooses the registers. The macros do not touch any other register.

#define DK_GAIN_STEP(OUT, IN1, GN) OUT = IN1 * GN

#define DK_MIX_STEP(OUT, IA, IB, MIXV, TMP) \
    TMP = IB - IA; \
    TMP = TMP * MIXV; \
    OUT = IA + TMP
