/* core/runtime: data anchors read only by storage_adapter.rs. */
    .section .rodata
    .even

    /* Read-only sample filesystem entry points, resolved through the current
     * port's hash declarations. Kept in this DT2 object, never the shared Rust
     * object's required imports (DN2 has no path filesystem). */
    .balign 4
    .globl dkrt_sample_fs
dkrt_sample_fs:
    .long cf.storage.fn.path.open
    .long cf.storage.fn.path.read
    .long cf.storage.fn.path.close
    .long cf.storage.fn.path.size

    .balign 4
    .globl dkrt_files_fs
dkrt_files_fs:
    .long cf.storage.fn.directory.open
    .long cf.storage.fn.directory.next
    .long cf.storage.fn.directory.metadata
