// digikit DSP kernels: linear ramp over one block, to smooth a parameter between two blocks.
//
// DK_RAMP_SETUP(STEP, CUR, TGT, INVN)
//   maths:         step = (target - cur) / n, as a multiply by 1/n (n = 32 = DK_BLOCK, so INVN = 1/32 = 0.03125 = 0x3d000000)
//   arguments:     STEP = step (result), CUR = cur, TGT = target, INVN = 1/n (the caller loads it before the loop)
//                  (all F registers)
//   instructions:  2, once per block
//   accuracy:      two float32 roundings: step = target - cur, step = step 1/n
//
// DK_RAMP_STEP(CUR, STEP)
//   maths:         cur = cur + step; after n steps cur is target to within the rounding drift
//   arguments:     CUR = the running value (in and out), STEP = step
//   instructions:  1 (it can share an instruction with a multiply: F0 = F1 * F2, F3 = F3 + F4)
//   accuracy:      one float32 rounding per step; the drift is at most about n ulp of the target. Store the exact
//                  target (not CUR) as the next block's start value, so the drift does not accumulate.
//
// The first value used is cur + step (the ramp does not repeat the start value).

#define DK_RAMP_SETUP(STEP, CUR, TGT, INVN) \
    STEP = TGT - CUR; \
    STEP = STEP * INVN

#define DK_RAMP_STEP(CUR, STEP) CUR = CUR + STEP
