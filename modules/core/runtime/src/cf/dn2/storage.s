/* core/runtime: data anchors read only by storage_adapter.rs. */
    .section .rodata
    .even

    /* DN2 has no stock path filesystem. Shared Rust checks the table before
     * converting any entry into a callable function pointer. */
    .balign 4
    .globl dkrt_sample_fs
dkrt_sample_fs:
    .long 0, 0, 0, 0

    .balign 4
    .globl dkrt_files_fs
dkrt_files_fs:
    .long 0, 0, 0
