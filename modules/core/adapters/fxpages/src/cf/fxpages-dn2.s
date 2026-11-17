| core/fxpages on the Digitone II (1.12): the data table of the FX view pages of run-time effects.
| Design: docs/design/digikit-registry.md ("Effect pages"), docs/findings/61-fx-pages.md. The numbers (device id, page numbers, effect slots, value
| indices) are the device file's, through build/gen/device-dn2.inc (`patchkit gen`).
|
| The image holds data only. The hooks of the effect pages (the page accessor, the FX view's page list, the tab label, the filter graph, the send page's list, the sound
| load count and the three kit hooks) are in the service file `fx.dks` (modules/core/services/fx/dks): it installs them at their stock sites when it is loaded and reads
| this table through the core (`CoreApi::info`, the import `dkc_info`), so the runtime and the image know nothing of the hooks' code. This table has what the hooks and the
| service need from the firmware and nothing that changes at run time: the state words (rows, page list, counters) are 0 here; the service keeps them in its own bss and
| fills them into its copy of the table (`fxpage::bind_table`).
|
| The return words (196 and up) are where each hook continues in the stock code: the site plus the length of the instructions the hook replaced, or another place of
| the same routine (`_fp_label + 0x1e`, `_fp_graph + 0x1c4`).
|
| fxpages_info is the table the service reads (all words big endian).

    .include "device-dn2.inc"

    .equ ROW_SIZE, 0x2c         | the firmware's page descriptor row: name, title (char pointers), 9 codes
    .equ LIST_WORDS, 8          | the list buffer the service keeps: the stock page numbers and the effect pages
    .equ FXP_MARK, 0x46a7       | the value of the mark in the array: the rows hold values (any other value: the array was not given its defaults)

    .section .rodata
    .balign 4
    .globl fxpages_info
fxpages_info:
    .long   0x46585047                  |  0 'FXPG'
    .long   3                           |  4 version (3: the return words from byte 196; 2: the send page words from byte 156)
    .long   DEVICE_ID                   |  8 device ([digikit] id: 2 = dn2)
    .long   0                           | 12 the page rows (the service's state)
    .long   ROW_SIZE                    | 16 row size
    .long   FX_PAGE_FIRST               | 20 the page number of row 0
    .long   LIMIT_FX_SLOTS              | 24 rows (one for each effect slot)
    .long   0                           | 28 the list of page numbers the FX view gets (the service's state)
    .long   LIST_WORDS                  | 32 its capacity
    .long   0                           | 36 the count cell (the service's state)
    .long   _fp_stock                   | 40 the stock list (page numbers, words)
    .long   _fp_stock_n                 | 44 the address of `moveq N,D1` in the builder: N is the byte at +1, the stock count
    .long   _fp_p_first                 | 48 the parameter code of knob 0 of effect row 0 (`params.fp0`)
    .long   8                           | 52 parameter rows for each effect row: 7 knobs and MIX
    .long   FX_ARRAY_OFF                | 56 byte offset of the kit's effect value array in the kit object
    .long   FX_VALUES_N                 | 60 number of value indices
    .long   FX_TPL_PERCENT              | 64 stock parameter codes whose rows the knobs copy
    .long   FX_TPL_BIPOLAR              | 68
    .long   FX_TPL_MIX                  | 72
    .long   FX_KIT_PTR                  | 76 the address of the firmware's pointer to the live kit object
    .byte   FX_VALUE_0, FX_VALUE_1, FX_VALUE_2, FX_VALUE_3, FX_VALUE_4, FX_VALUE_5, FX_VALUE_6, FX_VALUE_7           | 80 the value indices (32 bytes)
    .byte   FX_VALUE_8, FX_VALUE_9, FX_VALUE_10, FX_VALUE_11, FX_VALUE_12, FX_VALUE_13, FX_VALUE_14, FX_VALUE_15
    .byte   FX_VALUE_16, FX_VALUE_17, FX_VALUE_18, FX_VALUE_19, FX_VALUE_20, FX_VALUE_21, FX_VALUE_22, FX_VALUE_23
    .byte   FX_VALUE_24, FX_VALUE_25, FX_VALUE_26, FX_VALUE_27, FX_VALUE_28, FX_VALUE_29, FX_VALUE_30, FX_VALUE_31
    .long   0                           | 112 the effect ID of each row (the service's state)
    .long   0                           | 116 the default raw value of each value (the service's state)
    .long   0                           | 120 the array the last kit load wrote (the service's state)
    .long   0                           | 124 kit loads that went through the hook (the service's state)
    .long   0                           | 128 kit saves that went through the hook (the service's state)
    .long   0                           | 132 the array the last save read (the service's state)
    .long   0                           | 136 the storage record the last load read (the service's state)
    .long   0                           | 140 the storage record the last save wrote (the service's state)
    .long   0                           | 144 kit initialisations that went through the init hook (the service's state)
    .long   FX_MARK_INDEX               | 148 the array index of the mark
    .long   FXP_MARK                    | 152 the mark's value
    .long   LIMIT_SEND_SLOTS            | 156 send cells (knobs of the send page; effects that can run on a send bus)
    .long   0                           | 160 the send page's row (the service's state)
    .long   0                           | 164 the count cell of the track FX key's list (the service's state)
    .long   _fp_p_send                  | 168 the parameter code of send cell 0 (`params.fs0`); cell c is + c
    .long   FX_SEND_IDX_0               | 172 the Sound array index of cell 0
    .long   FX_SEND_IDX_1               | 176 and of cell 1
    .long   FX_SEND_TEMPLATE            | 180 the stock send row whose owner and key words a send knob copies
    .long   FX_SOUND_OFF                | 184 the first Sound object in the kit object (bytes from the kit object)
    .long   FX_SOUND_STRIDE             | 188 bytes between the 16 Sound objects
    .long   0                           | 192 sound loads that went through the loader hook (the service's state)
    .long   _fp_acc + 6                 | 196 page_desc: the stock code behind the hook
    .long   _fp_list + 6                | 200 fx_view_list.load
    .long   _fp_keylist + 6             | 204 fx_key_list.load
    .long   _fp_sound + 8               | 208 sound_load_exit
    .long   _fp_label + 0x1e            | 212 tab_label_test: the tail of the label code (pushes the position and draws)
    .long   _fp_label + 6               | 216 tab_label_test: the stock compare and its `bne.b`
    .long   _fp_graph + 0x1c4           | 220 filter_graph_test: the target of the stock `beq.w` (no graph)
    .long   _fp_graph + 6               | 224 filter_graph_test: the graph is drawn
    .long   _fp_load + 6                | 228 fx_array.load
    .long   _fp_save + 6                | 232 fx_array.save
    .long   _fp_init + 8                | 236 fx_array.init_exit
    .long   _fp_keystock                | 240 the stock list of the track FX key
    .long   FX_TRACK_PAGE               | 244 the track FX page number
    .long   FX_TAIL_OFF                 | 248 byte offset of the last word of the array that stock moves with the storage record
    .long   FX_STORE_OFF                | 252 byte offset of the spare halfwords in the storage record
