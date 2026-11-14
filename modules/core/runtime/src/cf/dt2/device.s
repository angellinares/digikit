/* core/runtime: the device object. The id, the name, the store area and the PEEK ranges are the device file's (digitakt-ii.toml [digikit] id, name and peek) through
 * build/gen/device-dt2.inc, which `patchkit gen` writes. */
    .include "device-dt2.inc"
    .section .rodata
    .even
    .globl dkrt_device_id
dkrt_device_id:
    .long DEVICE_ID
    .globl dkrt_device_name
dkrt_device_name:
    .long DEVICE_NAME_WORD
    /* the capabilities the runtime branches on instead of the device id: [capabilities] card = "slots" (the samples are store files) and
     * [digikit] sample_path_max */
    .globl dkrt_samples_in_store
dkrt_samples_in_store:
    .long SAMPLES_IN_STORE
    .globl dkrt_sample_path_max
dkrt_sample_path_max:
    .long SAMPLE_PATH_MAX
    /* the DSP load the broker checks a selection against: [adapter.machine] voices and the headroom (instructions per block) of dkprof::budget */
    .globl dkrt_dsp_headroom
dkrt_dsp_headroom:
    .long DSP_HEADROOM
    .globl dkrt_dsp_voices
dkrt_dsp_voices:
    .long DSP_VOICES
    .globl dkrt_peek_n
dkrt_peek_n:
    .long PEEK_N
    .globl dkrt_sound_words
dkrt_sound_words:
    .long FX_SOUND_WORDS
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
