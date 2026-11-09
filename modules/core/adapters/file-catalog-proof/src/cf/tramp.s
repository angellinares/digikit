/* Run in the real UI task, preserving the original queue-pop call ABI. */
    .include "dk_fw_view.inc"
    .text
    .even
    .globl catalog_loop_hook
catalog_loop_hook:
    dk_fw_view_ui_loop_hook catalog_tick, catalog_queue_pop
