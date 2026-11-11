// digikit DSP kernels: the block-routine calling convention (the stock / selcc C convention).
//
// A block routine is callable from assembly (DK_CALL) and from selcc C. Sources, all [D] unless marked:
//   docs/findings/20-patch-toolchains.md:76-78  frame pointer I6, return address at [I6 - 1], arguments R4/R8/R12, result R0,
//                                               symbols with a trailing dot (selcc: `scale.`)
//   docs/findings/53-stock-dsp-shape.md:14-20   CJUMP call, prologue `I7 = MODIFY(I7, -N)`, saves at `DM(I6 - n)`, return
//                                               `I12 = DM(I6 - 1)` + `JUMP (M14, I12) (DB)` + `RFRAME`, callee-saved set
//   docs/findings/05-sharc-isa-and-decoding.md:125-139  the call is `CJUMP (DB)` + `DM(I7,M7) = R2` + `DM(I7,M7) = PC`
//   docs/findings/07-emulator.md:4466, 06-sharc-engine-and-startup.md:2662  CJUMP: R2 = I6, I6 = I7; RFRAME: I7 = I6, I6 = DM(0, I6)
//   tests/patchkit/test_pk_sharc.py:27, 106  frame layout the selcc tests build by hand: [I6] = caller I6, [I6 - 1] = return address - 1
//
// Arguments: R4, R8, R12 (R4 first). No result (R0 is not defined on return for a void routine).
// Needs M5 = 0, M6 = 1, M7 = -1, M14 = 1 (the module contract holds them); the routine never writes them.
//
// Frame (stack grows down, I7 is the stack pointer, I6 the frame pointer):
//   [I6]        caller's I6            (pushed by DK_CALL)
//   [I6 - 1]    explicit continuation - 1 (pushed by DK_CALL)
//   [I6 - 2] .. [I6 - 1 - N]   N save slots of the routine (DK_SAVE slot numbers 2 .. N + 1)
//   I7 = I6 - 2 on entry, I6 - 2 - N after DK_ENTER(N); DK_LEAVE's RFRAME sets I7 = I6 and I6 = [I6].
//
// Caller must assume clobbered: R0, R1, R2, R4, R8, R12, I4, I12, MR, ASTAT, the loop counter, and the stack below I7.
// Preserved by a routine (callee-saved): R3, R5-R7, R9-R11, R13-R15, I0-I3, I5, M1-M3, B3, and everything the module
// contract keeps (I6-I15 on return, M0, M5-M15, L0-L7, MODE1). A routine saves what it uses of these (DK_SAVE) and restores
// it before DK_LEAVE. A routine may keep R12 unchanged, but a caller must not rely on it.
//
// Routine symbols: `_dk_<name>` for assembly (DK_CALL) and `dk_<name>.` for selcc C (the dot is selcc's mangling). Both name
// the same address. In C: `void dk_biquad(int x, int y, int st);` with normal-word addresses (see biquad.inc).
//
// Cost in instructions: DK_CALL 3 (CJUMP and two delay slots). DK_ENTER 1, DK_SAVE 1 each, DK_RESTORE 1 each, DK_RETADDR 1,
// DK_LEAVE 3 (jump and two delay slots, one of them a restore). With S saved registers: 3 + 1 + S + 1 + (S - 1) + 3 = 2 S + 7.
// The immediate return-label store assembles as Type16a post-modify. Do not
// store PC through Type3b: that source assembled as pre-modify and native PC
// reads did not provide the required return value. Labels must be unique in
// the assembled object, including repeated calls to the same routine.
//
// DK_CALL(fn, label)    call; unique continuation label; arguments in R4, R8, R12
// DK_ENTER(N)           first instruction of a routine: reserve N save slots
// DK_SAVE(SLOT, REG)    store REG in save slot SLOT (2 .. N + 1)
// DK_RESTORE(REG, SLOT) reload REG from SLOT
// DK_RETADDR            load I12 with the return address; put it at least 4 instructions before DK_LEAVE (DAG load-to-use)
// DK_LEAVE(REG, SLOT)   return: JUMP (M14, I12) (DB), REG = [I6 - SLOT] in the first delay slot, RFRAME in the second
//
// The caller writes the trailing ";" after each macro. Include this file once per module, before the routine files.

#define DK_CALL(fn, RETURN_LABEL) \
    CJUMP fn (DB); \
    DM(I7, M7) = R2; \
    DM(I7, M7) = RETURN_LABEL - 1; \
    RETURN_LABEL:

#define DK_ENTER(N) MODIFY(I7, -N) (NW)

#define DK_SAVE(SLOT, REG) DM(-SLOT, I6) = REG

#define DK_RESTORE(REG, SLOT) REG = DM(-SLOT, I6)

#define DK_RETADDR I12 = DM(-1, I6)

#define DK_LEAVE(REG, SLOT) \
    JUMP (M14, I12) (DB); \
    REG = DM(-SLOT, I6); \
    RFRAME
