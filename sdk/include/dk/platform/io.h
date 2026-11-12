/* <dk/platform/io.h> (api 1.10; the imports dk_io_post, dk_io_cancel and dk_io_wake since 1.11, written api->io_post etc. in the old entry): run work on the runtime's own I/O task.
 *
 * step(arg) runs on that task (priority 3: below the UI task, so a long transfer does not slow the UI). It may block and it may call
 * api->file_* (<dk/platform/file.h>), which answer E_CONTEXT anywhere else. It returns:
 *   DK_IO_MORE  call me again (other jobs get their turn first: round robin),
 *   DK_IO_WAIT  sleep until api->io_wake(id), from an interrupt handler, an adapter or any task,
 *   0           finished,
 *   < 0         failed (the error is the status of `done`).
 * done(arg, status) is called once in the UI task with the last result, or DK_IO_E_CANCELLED after io_cancel. A module that is unloaded
 * gets no `done`; its running step is cancelled and the runtime keeps the module's memory until the step has returned.
 *
 * The task wakes once for several posts and wakes: after each wake it runs jobs until none is runnable, so a step must find all its
 * work (a wake that comes while it runs is kept: its next DK_IO_WAIT does not sleep). A step that returns DK_IO_MORE all the time takes all the CPU
 * the UI and MIDI tasks leave. The job table has 8 entries (io_post: E_BUSY when full). io_post is for task context, not an interrupt.
 *
 * There is one I/O task and it runs one step at a time: a step that never returns (an endless loop, or a wait on something other than
 * the file calls) blocks every other module's I/O jobs. Do a bounded amount of work per step and return DK_IO_MORE or DK_IO_WAIT.
 *
 * A file session that a job leaves open when it ends (finished, failed or cancelled) is aborted by the runtime.
 *
 * The values must match modules/core/runtime/src/rs/src/io.rs. */
#ifndef DK_IO_H
#define DK_IO_H

#include <dk/core/api.h>

#define DK_IO_MORE 1
#define DK_IO_WAIT 2
#define DK_IO_E_CANCELLED (-81)

#endif
