// digikit DSP kernels: one-pole low-pass.
//
// DK_ONEPOLE_STEP(OUT, IN1, COEF, TMP)
//   maths:         y = y + a (x - y); the state is y itself, so OUT is both the old and the new value
//   arguments:     OUT = y (state in, output out), IN1 = x, COEF = a (0 < a <= 1), TMP = temporary (all F registers)
//   instructions:  3 (subtract, multiply, add; a dependent chain)
//   accuracy:      three float32 roundings, in this order: t = x - y, t = a t, y = y + t
//
// The caller writes the trailing ";". For a = 1 - exp(-2 pi fc / fs): fc = 1 kHz at 48 kHz gives a = 0.1227.

#define DK_ONEPOLE_STEP(OUT, IN1, COEF, TMP) \
    TMP = IN1 - OUT; \
    TMP = COEF * TMP; \
    OUT = OUT + TMP
