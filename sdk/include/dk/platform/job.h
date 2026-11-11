/* Background jobs (API 1.8): api->job_post / api->job_cancel run work on the firmware's loader worker task.
 *
 * step(ctx) returns DK_JOB_MORE to be called again, 0 when finished, or a negative error. done(ctx, status) is called once
 * in the UI task with the last result, or DK_JOB_E_CANCELLED after a cancel. A step runs in the worker task: it computes on
 * its own memory and calls no dk_api function. The worker is shared with the stock sample loads, so a step should take
 * well under 10 ms; a long job is many steps.
 *
 * The values must match modules/core/runtime/src/rs/src/worker.rs. */
#ifndef DK_JOB_H
#define DK_JOB_H

#include <dk/core/api.h>

#define DK_JOB_MORE 1
#define DK_JOB_E_CANCELLED (-80)

#endif
