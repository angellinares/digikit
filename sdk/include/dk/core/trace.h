/* <dk/core/trace.h>: log marks that exist only in a traced build. Hand-written.

   DK_TRACE_LOG(msg, value);    dk_log(DK_LOG_TRACE, msg, value): a message (24 bytes kept) and a word in the runtime's log ring.

   `patchkit pack --trace` (also `module:check -- --trace`, `dk:dev -- --trace`) compiles with -DDK_TRACE. Without it the macro
   compiles to nothing: no code, no data, no import, the same object. The arguments are not evaluated then, but they still
   count as used, so a variable that only a DK_TRACE_LOG reads gives no warning. It is a macro, not a function, for that reason.

   DK_SPAN_BEGIN(owner); ...; DK_SPAN_END(owner);    measures the cycles of the code between them and hands (owner, cycles) to the span sink (<dk/core/sink.h>).
   Only the dispatcher of a kind (a service) uses it, around its per-row call; a content module or an app may not (`mise run check:layers`, rule spans).
   It is always compiled in. With no sink set, each of the two macros costs one load and one branch on the sink's pointer, and `owner` is not evaluated
   (the cycle counter is not read either). BEGIN declares the local `dk_span_t0_`, so one pair fits a block: put a second pair in a block of its own, and do
   not nest spans of one block. The cycle counter and the emit are the runtime's: `dk_span_sink` (the span channel's sink pointer, a data import), `dk_span_clock`
   and `dk_span_emit` become imports of sdk/sdk.toml with their producers (profiling plan, P8a); nothing uses them before that.
   The DSP side has the same macros in dk_trace.inc and the stats window of the layout below. */
#ifndef DK_TRACE_H
#define DK_TRACE_H
#include <dk/core/log.h>

/* The runtime's side of DK_SPAN. */
extern volatile uint32_t dk_span_sink;
uint32_t dk_span_clock(void);
void dk_span_emit(uint32_t owner, uint32_t cycles);

#define DK_SPAN_BEGIN(owner)                                                                       \
    uint32_t dk_span_t0_ = 0;                                                                      \
    if (dk_span_sink)                                                                              \
        dk_span_t0_ = dk_span_clock();                                                             \
    (void)sizeof(owner)
#define DK_SPAN_END(owner)                                                                         \
    do {                                                                                           \
        if (dk_span_sink)                                                                          \
            dk_span_emit((owner), dk_span_clock() - dk_span_t0_);                                  \
    } while (0)

/* The stats window of the DSP side (the layout of dk_trace.inc, the same numbers): words in DSP data memory, written by the stub's `_dk_span_add`, read from the
   ColdFire side through the window registered with dsp-link. The slot `_dk_stats_ptr` holds the window's word address; 0 means no window and nothing is measured.
   word 0: DK_STATS_MAGIC, 1: DK_STATS_VERSION, 2: DK_STATS_ROWS, 3: blocks measured since the window was cleared; then DK_STATS_ROWS rows of
   DK_STATS_ROW_WORDS words: owner row number, calls, sum of cycles, largest single span. A counter is a 32-bit word that wraps; the reader takes differences. */
#define DK_STATS_MAGIC 0x444b5357u /* "DKSW" */
#define DK_STATS_VERSION 1u
#define DK_STATS_HEAD_WORDS 4u
#define DK_STATS_ROW_WORDS 4u
#define DK_STATS_ROWS 16u

#ifdef DK_TRACE
#define DK_TRACE_LOG(msg, value) dk_log(DK_LOG_TRACE, (msg), (uint32_t)(value))
#else
#define DK_TRACE_LOG(msg, value) ((void)(0 && (value)))
#endif

#endif
