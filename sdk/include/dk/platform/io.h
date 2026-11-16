/* <dk/platform/io.h> (the imports dk_io_post, dk_io_cancel, dk_io_wake, dk_helper_run and dk_helper_wait): run work on the runtime's own I/O task.
 *
 * step(arg) runs on that task (priority 3: below the UI task, so a long transfer does not slow the UI). It may block and it may call
 * dk_files_create/write/close/abort/delete (<dk/platform/files.h>), which answer E_CONTEXT anywhere else. It returns:
 *   DK_IO_MORE  call me again (other jobs get their turn first: round robin),
 *   DK_IO_WAIT  sleep until dk_io_wake(id), from an interrupt handler, an adapter or any task,
 *   0           finished,
 *   < 0         failed (the error is the status of `done`).
 * done(arg, status) is called once in the UI task with the last result, or DK_IO_E_CANCELLED after io_cancel. A module that is unloaded
 * gets no `done`; its running step is cancelled and the runtime keeps the module's memory until the step has returned.
 *
 * The task wakes once for several posts and wakes: after each wake it runs jobs until none is runnable, so a step must find all its
 * work (a wake that comes while it runs is kept: its next DK_IO_WAIT does not sleep). A step that returns DK_IO_MORE all the time takes all the CPU
 * the UI and MIDI tasks leave. The job table has 8 entries (dk_io_post: E_BUSY when full). dk_io_post is for task context, not an interrupt.
 *
 * There is one I/O task and it runs one step at a time: a step that never returns (an endless loop, or a wait on something other than
 * the file calls) blocks every other module's I/O jobs. Do a bounded amount of work per step and return DK_IO_MORE or DK_IO_WAIT.
 *
 * A file session that a job leaves open when it ends (finished, failed or cancelled) is aborted by the runtime.
 *
 * The helper task (runtime 0.8.3): while a step blocks in a file call, `dk_helper_run(work, arg)` lets `work(arg)` run on the runtime's helper task (priority 3,
 * 4 KiB stack), and `dk_helper_wait()` collects it. Both are I/O-step calls (DK_E_CONTEXT elsewhere). `work` is pure compute on memory the step owns and the
 * step does not touch meanwhile: it calls no import, does not wait and keeps to 4 KiB of stack. There is one helper and one run at a time: dk_helper_run
 * returns DK_E_BUSY while a run is uncollected, and DK_E_NOT_FOUND when the kernel gave no task; the step then runs `work` itself. The runtime's own use is
 * the read-back hash inside dk_files_close, which never overlaps the step's own calls: a run that is still uncollected when dk_files_close starts makes the
 * close hash on its own (slower, same result). Collect the run in the same step (a run left behind is collected when the step returns). Example, copying
 * while a card write blocks:
 *
 *     dk_helper_run(copy_job, &job);      // E_BUSY: call copy_job(&job) here
 *     dk_files_write(files, h, off, buf, len);
 *     dk_helper_wait();                   // E_STATE when dk_helper_run did not start one
 *
 * The values must match modules/core/runtime/src/rs/src/io.rs. */
#ifndef DK_IO_H
#define DK_IO_H

#include <dk/core/module.h>

#define DK_IO_MORE 1
#define DK_IO_WAIT 2
#define DK_IO_E_CANCELLED (-81)

#endif
