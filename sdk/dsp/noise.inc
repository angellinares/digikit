// digikit DSP kernels: integer xorshift32 (13, -17, 5).
//
// DK_XORSHIFT32_STEP(STATE, TMP, RIGHT)
//   arguments: STATE = unsigned 32-bit state, updated in place; TMP = scratch R register;
//              RIGHT = R register preloaded with -17. All three must be distinct.
//   instructions: 6. Logical shifts discard overflow; zero stays zero.
//   clobbers: STATE, TMP and arithmetic flags. No seed or float conversion is supplied.
//
// DK_XORSHIFT32_LOAD_STEP(STATE, TMP)
//   Same recurrence, 7 instructions; loads -17 into TMP at the original BD schedule point.
//   STATE and TMP must be distinct. Clobbers both and arithmetic flags.
//
// These schedules preserve BD and pluck's instruction order. Caller owns nonzero seeds,
// state storage and signed float conversion/scaling. Write the trailing semicolon.

#define DK_XORSHIFT32_STEP(STATE, TMP, RIGHT) \
    TMP = LSHIFT STATE BY 13; \
    STATE = STATE XOR TMP; \
    TMP = LSHIFT STATE BY RIGHT; \
    STATE = STATE XOR TMP; \
    TMP = LSHIFT STATE BY 5; \
    STATE = STATE XOR TMP

#define DK_XORSHIFT32_LOAD_STEP(STATE, TMP) \
    TMP = LSHIFT STATE BY 13; \
    STATE = STATE XOR TMP; \
    TMP = -17; \
    TMP = LSHIFT STATE BY TMP; \
    STATE = STATE XOR TMP; \
    TMP = LSHIFT STATE BY 5; \
    STATE = STATE XOR TMP
