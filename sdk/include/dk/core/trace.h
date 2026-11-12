/* <dk/core/trace.h>: log marks that exist only in a traced build. Hand-written.

   DK_TRACE_INIT(api);          once per source file, in dkm_init, with the dk_api pointer it received.
   DK_TRACE_LOG(msg, value);        dk_api.log(msg, value): a message (24 bytes kept) and a word in the runtime's log ring.

   `patchkit pack --trace` (also `module:check -- --trace`, `dk:dev -- --trace`) compiles with -DDK_TRACE. Without it both macros
   compile to nothing: no code, no data, no import, the same object. The arguments of DK_TRACE are not evaluated then, but they still
   count as used, so a variable that only a DK_TRACE reads gives no warning. */
#ifndef DK_TRACE_H
#define DK_TRACE_H
#include <dk/core/api.h>

#ifdef DK_TRACE
static const dk_api *dk_trace_api __attribute__((unused));
#define DK_TRACE_INIT(api) ((void)(dk_trace_api = (api)))
#define DK_TRACE_LOG(msg, value)                                                                   \
    ((void)(dk_trace_api ? (dk_trace_api->log((const uint8_t *)(msg), (uint32_t)(value)), 0) : 0))
#else
#define DK_TRACE_INIT(api) ((void)(api))
#define DK_TRACE_LOG(msg, value) ((void)(0 && (value)))
#endif

#endif
