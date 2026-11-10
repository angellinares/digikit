/* core/usb-bulk: the board fact the buffers need: the uncached alias of DDR lies this far above the cached window (devices/*.toml [board],
 * written to build/gen/device.inc by `patchkit gen`). */
    .include "device.inc"
    .section .rodata
    .even
    .globl usb_alias_delta
usb_alias_delta:
    .long DDR_UNCACHED_ALIAS - DDR_CACHED_BASE
    .globl usb_ddr_base
usb_ddr_base:
    .long DDR_CACHED_BASE
    .globl usb_ddr_bytes
usb_ddr_bytes:
    .long DDR_BYTES
