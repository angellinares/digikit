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
