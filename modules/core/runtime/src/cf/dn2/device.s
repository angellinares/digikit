/* core/runtime: the device object. The id, the name, the store area and the PEEK ranges are the device file's (digitone-ii.toml [digikit] id, name and peek) through
 * build/gen/device-dn2.inc, which `patchkit gen` writes. */
    .include "device-dn2.inc"
    .section .rodata
    .even
    .globl dkrt_device_id
dkrt_device_id:
    .long DEVICE_ID
    .globl dkrt_device_name
dkrt_device_name:
    .long DEVICE_NAME_WORD
    .globl dkrt_peek_n
dkrt_peek_n:
    .long PEEK_N
    .globl dkrt_peek_ranges
dkrt_peek_ranges:
    .long PEEK_LO_0, PEEK_HI_0, PEEK_LO_1, PEEK_HI_1
    .long PEEK_LO_2, PEEK_HI_2, PEEK_LO_3, PEEK_HI_3
    /* the store area (docs/design/digikit-store.md): the first card sector and the number of sectors, [card.store] */
    .globl dkrt_store_first
dkrt_store_first:
    .long STORE_FIRST
    .globl dkrt_store_sectors
dkrt_store_sectors:
    .long STORE_SECTORS
    /* The image id (docs/design/digikit-upload.md, INFO): the marker, then 8 bytes that patchkit stamps when it makes the image
     * (packages/patchkit/src/imageid.rs). Zero in the object; INFO reports them. */
    .globl dkrt_image_id
    .ascii "DKIMGID1"
dkrt_image_id:
    .long 0, 0

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
