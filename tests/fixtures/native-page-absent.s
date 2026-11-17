/* Isolate the ordinary machine mapping tests from the optional callback-page provider. */
    .text
    .globl mf_native_page_count
mf_native_page_count:
    move.l 8(%sp),%d0
    rts
    .globl mf_native_descriptor
mf_native_descriptor:
    moveq #0,%d0
    rts
    .globl mf_native_code
mf_native_code:
    moveq #-1,%d0
    rts
    .globl mf_native_draw
mf_native_draw:
    moveq #0,%d0
    rts
    .globl mf_native_overlay
mf_native_overlay:
    rts
