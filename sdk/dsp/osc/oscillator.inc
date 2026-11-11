// digikit DSP kernels: BD/PD folded-phase sine polynomial.
//
// DK_SINE_POLY_STEP(OUT, X, SQUARE, COEF, OUT_BITS, COEF_BITS)
//   maths: -x * (a1 + x^2 * (a3 + x^2 * (a5 + x^2 * (a7 + x^2 * a9))))
//   arguments: X = folded phase in [-0.25, 0.25] (F register, preserved);
//              OUT, SQUARE, COEF = distinct output and scratch F registers, distinct from X;
//              OUT_BITS and COEF_BITS = R aliases of OUT and COEF respectively.
//   instructions: 16, in the original BD/PD order; no NOPs removed from the caller.
//   clobbers: OUT, SQUARE, COEF and arithmetic flags. No memory or other registers.
//   accuracy: float32 multiply/add with separate rounding, no fused operations.
//             The normalized phase fold is caller-owned; this is not phase wrapping.
//   reference: tools/patchkit/dsp_osc_ref.py; tests/patchkit/test_dsp_osc_kernels.py.
//
// Coefficients are loaded inline to preserve the consumers' schedules. Write the trailing semicolon.

#define DK_SINE_POLY_STEP(OUT, X, SQUARE, COEF, OUT_BITS, COEF_BITS) \
    SQUARE = X * X; \
    OUT_BITS = 0x42283c1a; \
    OUT = OUT * SQUARE; \
    COEF_BITS = 0xc2996966; \
    OUT = OUT + COEF; \
    OUT = OUT * SQUARE; \
    COEF_BITS = 0x42a335e3; \
    OUT = OUT + COEF; \
    OUT = OUT * SQUARE; \
    COEF_BITS = 0xc2255de7; \
    OUT = OUT + COEF; \
    OUT = OUT * SQUARE; \
    COEF_BITS = 0x40c90fdb; \
    OUT = OUT + COEF; \
    OUT = OUT * X; \
    OUT = -OUT
