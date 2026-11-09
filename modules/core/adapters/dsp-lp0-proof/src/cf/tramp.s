/* The existing UI-task call ABI: tick, then the original queue pop. */
    .include "dk_fw_view.inc"
    .text
    .even
    .globl lp_loop_hook
lp_loop_hook:
    dk_fw_view_ui_loop_hook lp_tick, lp_queue_pop
