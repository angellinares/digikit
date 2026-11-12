/* <dk/core/time.h>: the block clock (dk_time_blocks). Hand-written. Include <dk/core/module.h> first.

   dk_time_blocks() counts the DSP's blocks since boot, 1500 a second, and wraps after about 33 days. Subtract two readings as unsigned numbers. It
   reads one word and is callable from any task. It is 0 when the image has no block counter and no core/health. */
#ifndef DK_TIME_H
#define DK_TIME_H
#include <dk/core/module.h>

#define DK_BLOCKS_PER_SECOND 1500u
#endif
