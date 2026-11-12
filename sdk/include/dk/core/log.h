/* <dk/core/log.h>: the runtime's log (dk_log, dk_log_event). Hand-written. Include <dk/core/module.h> first.

   dk_log(level, "message", value) appends a message (NUL-terminated, 24 bytes kept) and one word to the runtime's log ring and, when the debug module is
   loaded, to its log with the module as the source. It is callable from any task (not from an interrupt). The levels are the log's own: an error
   is kept even when debug is off. dk_log_event(kind, flags, a) logs a typed event as the runtime's own calls do (see [log] of sdk/sdk.toml). */
#ifndef DK_LOG_H
#define DK_LOG_H
#include <dk/core/module.h>

#define DK_LOG_ERROR 0u
#define DK_LOG_WARN 1u
#define DK_LOG_INFO 2u
#define DK_LOG_DEBUG 3u
#define DK_LOG_TRACE 4u

#endif
