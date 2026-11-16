| Host test fixture: the table `machines_info` (as `MF_TAB` of the service file) for the DN2 hooks of core/machines, with the exits as the stubs of the test unit
| (`pk_cfunit`): `_mf_*` are symbols the unit gives (registry values, the end of each type reader call) or stubs (the stock code the hook continues in).
    .data
    .balign 4
    .globl MF_TAB
MF_TAB:
    .long   0x4d414348, 1, 2
    .long   0, 0, 0
    .long   _mf_first, _mf_count, _mf_entries, _mf_rows
    .long   mf_ensure
    .long   mf_native_page_count, mf_native_descriptor, 0, 0, 0
    .long   0, 0, _mf_pgrows, _mf_pgn
    .long   0, 0
    .long   0
    .long   0, 0
    .long   _mf_gnext, _mf_gtaken
    .long   _mf_ft + 6, _mf_rd + 8, _mf_ow + 6
    .long   _mf_rc0 + 4, _mf_rc1 + 4, _mf_rc2 + 6, _mf_rc3 + 6, _mf_rc4 + 6, _mf_rc5 + 6, _mf_rc6 + 6, _mf_rc7 + 6, _mf_rc8 + 6
