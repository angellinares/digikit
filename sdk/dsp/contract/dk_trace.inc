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
