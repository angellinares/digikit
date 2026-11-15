/* <dk/platform/debug.h>: read-only views for the debug and probe modules (dk_debug_*). Hand-written.

   dk_debug_peek(i, out): the RAM ranges `[lo, hi)` that the device file's `[digikit] peek` allows, for a module that implements PEEK.
   dk_debug_poll(slot, out_fn): the owner and function of poll slot 0 to 3 of dkrt_poll_add (0: empty; 1 + the module record; 0xff: the firmware image), for a profiler.
   dk_debug_track_type(track): the machine type of a track of the active kit, read from the firmware.
   dk_debug_selected_params(out, n): the selected track and its first `n` Sound parameter words, read from the firmware's own accessors.
   dk_debug_log_sink_set(sink, tick): the one log sink (every event the runtime logs, before any filter, in whatever task logs, with interrupts
   masked) and its per-pass tick. A module that sets a sink clears it in `fini`.
   dk_debug_speed_flags(clear, set): the run-time switches of the speed levers (one RAM word, set at boot, never saved): clears the bits of `clear`, then
   sets the bits of `set` (both 0 only reads) and returns the new word. Bit 0: hash once; bit 2: the read-back hash overlaps the next card read. */
#ifndef DK_DEBUG_H
#define DK_DEBUG_H
#include <dk/core/module.h>
#endif
