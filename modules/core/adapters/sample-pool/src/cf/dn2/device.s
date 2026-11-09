| core/sample-pool on the Digitone II: the device's numbers for pool.c.
| They come from [adapter.sample_pool] of devices/digitone-ii.toml, through build/gen/sample-pool-dn2.inc (`patchkit gen`).

    .include "sample-pool-dn2.inc"

    .section .rodata
    .balign 4
    .globl sp_dev_ddr_base
sp_dev_ddr_base:
    .long   SP_DDR_BASE
    .globl sp_dev_ddr_bytes
sp_dev_ddr_bytes:
    .long   SP_DDR_BYTES
