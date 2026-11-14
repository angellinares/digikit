// dk_trace.inc: step marks for the fault record (docs: sdk/README.md, "Tracing"). Hand-written.
//
// DK_STEP(N, REG);   store the step number N (a small constant) in the word `_dk_step`, using the scratch register REG.
//
// The core clears `_dk_step` before each call into a module; the block tail copies it into the block's reply record. The fault record prints the step of the
// last block that ended if the module faults or hangs ("FX SEND STEP 3"); a step stored in the call that stops the DSP is lost with the core. SHARC+ cannot store an immediate to memory, so the macro needs a register that is free at that point.
// `patchkit pack --trace` defines DK_TRACE. Without it DK_STEP expands to nothing: no instruction, no import, the same object.
// The caller writes the trailing ";". Include this file once per module, before the routine files.

#ifdef DK_TRACE
.EXTERN _dk_step;
#define DK_STEP(N, REG) \
    REG = N; \
    DM(_dk_step) = REG
#else
#define DK_STEP(N, REG) ;
#endif

// ---- spans (docs/plans/sdk.md section 9, docs/plans/profiling.md section 7) ----
//
// DK_SPAN_BEGIN(SKIP, RA, RB);        reads the cycle counter into the word `_dk_span_t0`, when a stats window is registered.
// DK_SPAN_END(SKIP, ROW, RA, RB, RC); reads it again and hands (ROW, cycles) to the stub's `_dk_span_add`.
//
// Only the dispatchers of the image's stub use these, around the call of a module's entry (never a content module: `mise run check:layers`, rule spans). They are always
// assembled in. With no window (`_dk_stats_ptr` is 0) each costs three instructions, a load, a compare and a jump, and nothing else runs: the counter is not read
// (reading EMUCLK takes the emulator out of the fast tiers, `UNSAFE_READ_NAMES` of tools/sharc_dev/sharc_rsgen.py). The two macros take a label SKIP of their own and
// the scratch registers RA, RB (RC in END), which the stub's caller must be free to lose. One `_dk_span_t0`: spans do not nest.
//
// `_dk_span_add` (the stub's accumulator, R0 = row number 0 .. DK_STATS_ROWS-1, R1 = cycles; it clobbers R0 to R3, I0 and the flags) adds to the row `ROW`
// of the window `_dk_stats_ptr` points to: calls + 1, cycles summed, largest span kept.
//
// The window (the same numbers as <dk/core/trace.h> for the ColdFire side): word DK_STATS_W_MAGIC .. DK_STATS_W_BLOCKS, then DK_STATS_ROWS rows of DK_STATS_ROW_WORDS
// words in the order owner row, calls, cycles, largest. Words are 32-bit and wrap; the reader takes differences.
#define DK_STATS_MAGIC 0x444b5357
#define DK_STATS_VERSION 1
#define DK_STATS_W_MAGIC 0
#define DK_STATS_W_VERSION 1
#define DK_STATS_W_ROWS 2
#define DK_STATS_W_BLOCKS 3
#define DK_STATS_HEAD_WORDS 4
#define DK_STATS_ROW_WORDS 4
#define DK_STATS_ROWS 16
#define DK_STATS_ROW_OWNER 0
#define DK_STATS_ROW_CALLS 1
#define DK_STATS_ROW_CYCLES 2
#define DK_STATS_ROW_LARGEST 3

#define DK_SPAN_BEGIN(SKIP, RA, RB) \
    .EXTERN _dk_stats_ptr; \
    .EXTERN _dk_span_t0; \
    RA = DM(_dk_stats_ptr); \
    RA = PASS RA; \
    IF EQ JUMP SKIP; \
    RB = EMUCLK; \
    DM(_dk_span_t0) = RB; \
    SKIP:

#define DK_SPAN_END(SKIP, ROW, RA, RB, RC) \
    .EXTERN _dk_stats_ptr; \
    .EXTERN _dk_span_t0; \
    .EXTERN _dk_span_add; \
    RA = DM(_dk_stats_ptr); \
    RA = PASS RA; \
    IF EQ JUMP SKIP; \
    RB = EMUCLK; \
    RC = DM(_dk_span_t0); \
    R1 = RB - RC; \
    R0 = ROW; \
    CALL _dk_span_add; \
    SKIP:
