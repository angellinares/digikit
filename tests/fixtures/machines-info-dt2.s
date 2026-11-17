| Host test fixture: the table `machines_info` (as `MF_TAB` of the service file) for the DT2 hooks of core/machines, with the exits as the stubs of the test unit
| (`pk_cfunit`): `_mf_*` are symbols the unit gives (registry values) or stubs (the stock code the hook continues in).
    .data
    .balign 4
    .globl MF_TAB
MF_TAB:
    .long   0x4d414348, 1, 1
    .long   0, 0, 0
    .long   _mf_first, _mf_count, _mf_entries, _mf_rows
    .long   0
    .long   mf_native_page_count, 0, 0, 0, 0
    .long   _mf_spt, mf_slotmap, 0, 0
    .long   _mf_c5ne, _mf_c5eq
    .long   _mf_x_typearg
    .long   _mf_sp + 8, _mf_spa
    .space  4 * 17
