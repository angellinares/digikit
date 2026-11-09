| core/sample-pool on the Digitakt II: the device's numbers for pool.c.
| They come from [adapter.sample_pool] of devices/digitakt-ii.toml, through build/gen/sample-pool-dt2.inc (`patchkit gen`).

    .include "sample-pool-dt2.inc"

    .section .rodata
    .balign 4
    .globl sp_dev_slot_floor
sp_dev_slot_floor:
    .long   SP_SLOT_FLOOR
