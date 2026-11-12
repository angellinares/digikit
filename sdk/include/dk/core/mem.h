/* <dk/core/mem.h>: memory of the module arena (dk_alloc, dk_free). Hand-written. Include <dk/core/module.h> first.

   `void *dk_alloc(uint32_t size)` gives a 16-byte aligned block from the module arena, or NULL when it is full; the block is not zeroed. `void dk_free(void *p)` gives it back.
   The arena is shared by all modules and the runtime (512 KiB): ask for what the module needs in `init`, and keep a block for the life of the
   module rather than allocating in `tick`. A module's static data (bss) comes from the same arena when it loads. */
#ifndef DK_MEM_H
#define DK_MEM_H
#include <dk/core/module.h>
#endif
