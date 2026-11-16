| machines.dks on the Digitone II (1.12): the ColdFire routines the service's hooks run. They were in core/machines (`src/cf/dn2/machines.s`).
|
| Each hook is entered by a `jmp.l` that the runtime writes at its firmware site (`patchkit bind` made the hook raw: no trampoline); the frame type is entered
| through the image stub of the pointer slot `frametype` (an interrupt site). The routine takes the whole control flow of the site: it runs in place of the stock
| instructions there and ends in the stock code either by `jmp dk_tail_<hash of the hook>` (the displaced instructions of the site as in the user's firmware, made by `patchkit bind`) or, when a
| branch is among them, by pushing an exit address of the table `MF_TAB` and doing `rts` (the stack is as it was at the site). `MF_TAB` is the image table `machines_info` of core/machines, copied by `setup`
| (lib.rs). Offsets T_* below; the same layout is in hooks-dt2.s and in `image.rs`.
|
| Hook convention: registers the stock site frees are scratch (`devices/fw/machine.toml` lists them per hook); everything else a routine saves.
| The image keeps `mf_ensure` (the tables of the registry are built once, on first use: `mfn_call_ensure` calls it through the table), the machine list hook and the
| dispatch hook.

    .text
    .balign 4

| row layout (bytes), the field order of core/machines' [[registry.field]]
    .equ O_CLONE,  44
    .equ O_OWN,    52          | the DSP is told the entry's own type
    .equ ROWSIZE,  64
| row layout of the `pages` registry: name, title, nine codes, the machine's type, nine slots
    .equ PG_MACH,  44
    .equ PG_S,     48
    .equ PG_C,     8
    .equ PG_SIZE,  84


| the table (byte offsets of the words of MF_TAB; `image.rs` has the same layout)
    .equ T_FIRST, 24
    .equ T_COUNT, 28
    .equ T_ROWS, 36
    .equ T_ENSURE, 40
    .equ T_N_PAGE_COUNT, 44
    .equ T_N_DESCRIPTOR, 48
    .equ T_N_CODE, 52
    .equ T_N_DRAW, 56
    .equ T_N_OVERLAY, 60
    .equ T_PGROWS, 72
    .equ T_PGN, 76
    .equ T_R_GROUP_NEXT, 100
    .equ T_R_GROUP_TAKEN, 104
    .equ T_R_FT, 108
    .equ T_R_RD, 112
    .equ T_R_OW, 116
    .equ T_RC0, 120

| mf_ensure: the image's table builder; it preserves every register (not the flags). Called as a subroutine.
mfn_call_ensure:
    move.l  MF_TAB+T_ENSURE,-(%sp)
    rts                                 | enters mf_ensure, which returns to the caller of mfn_call_ensure

| mfn_row: d0 = index k (< count) -> a0 = &machines.rows[k]. Preserves every other register (not the flags).
mfn_row:
    move.l  %d1,-(%sp)
    move.l  #ROWSIZE,%d1
    muls.l  %d0,%d1
    add.l   MF_TAB+T_ROWS,%d1
    movea.l %d1,%a0
    move.l  (%sp)+,%d1
    rts

| ---- group helper (types 0..3 group 1, 4 group 2, anything else 0): every added type is group 1. The stock code compares d0 with 3 first; an added type is turned into 0
| (d0 is only the argument of the test). Site (6 bytes): `moveq #3,D1; cmp.l D0,D1; bge.b T`; GROUP_TAKEN is T, GROUP_NEXT the code behind the site.
    .globl mf_group
mf_group:
    move.l  %d0,%d1
    sub.l   MF_TAB+T_FIRST,%d1
    cmp.l   MF_TAB+T_COUNT,%d1
    bcc.s   1f
    moveq   #0,%d0
1:  moveq   #3,%d1                      | the stock test
    cmp.l   %d0,%d1
    blt.s   2f
    move.l  MF_TAB+T_R_GROUP_TAKEN,-(%sp)
    rts
2:  move.l  MF_TAB+T_R_GROUP_NEXT,-(%sp)
    rts

| ---- type maps: three functions index per-type tables built at start-up with the machine type they get as a stack argument, and fall back to MIDI's row above type 4. A
| machine with page rows (registry `pages`) answers them itself (page count, page descriptor, slot -> code from its rows); any other added type is replaced by its clone_of in
| the argument slot before the stock code loads it (the slot is the function's own parameter). Site: function entry; off = the argument's offset from the entry stack
| pointer. d0, d1, a0, a1 are free (a0 holds the slot in mf_typemap2).
    .macro CLONEMAP off                 | d0 = the type (>= first)
    lea     -12(%sp),%sp
    movem.l %d0-%d1/%a0,(%sp)
    sub.l   MF_TAB+T_FIRST,%d0
    cmp.l   MF_TAB+T_COUNT,%d0
    bcc.s   1f
    bsr.w   mfn_row
    move.l  O_CLONE(%a0),%d0
    move.l  %d0,(12 + \off,%sp)
1:  movem.l (%sp),%d0-%d1/%a0
    lea     12(%sp),%sp
    .endm

| FUN(type) -> the number of pages. Site (6 bytes): `moveq #4,D1; move.l 4(sp),D0`.
    .globl mf_typemap0
mf_typemap0:
    move.l  4(%sp),%d0
    cmp.l   MF_TAB+T_FIRST,%d0
    bcs.s   9f                          | a stock type (or a negative one: the range check below refuses it)
    movea.l %d0,%a1
    bsr.w   mfn_pg_count
    tst.l   %d0
    beq.s   1f
    move.l  %d0,-(%sp)
    move.l  %a1,-(%sp)
    movea.l MF_TAB+T_N_PAGE_COUNT,%a0
    jsr     (%a0)
    addq.l  #8,%sp
    rts                                 | ordinary pages plus an opt-in native callback page
1:  move.l  %a1,%d0
    CLONEMAP 4
9:  jmp     dk_tail_02e8a3f1            | the stock instructions of the site, made by `patchkit bind`, and on

| FUN(type, page) -> the page descriptor (D2 is saved by the stock code before the site). Site (8 bytes): `move.l 8(sp),D1; move.l 0xc(sp),D0`.
    .globl mf_typemap1
mf_typemap1:
    move.l  0xc(%sp),-(%sp)
    move.l  0xc(%sp),-(%sp)
    movea.l MF_TAB+T_N_DESCRIPTOR,%a0
    jsr     (%a0)
    addq.l  #8,%sp
    tst.l   %d0
    beq.s   8f
    move.l  (%sp)+,%d2
    rts
8:  move.l  8(%sp),%d0
    cmp.l   MF_TAB+T_FIRST,%d0
    bcs.s   9f
    movea.l %d0,%a1
    bsr.w   mfn_pg_count
    tst.l   %d0
    beq.s   1f
    move.l  %d0,%d1                     | count
    move.l  0xc(%sp),%d0                | page
    bmi.s   9f                          | out of range: the stock code answers MIDI's row (the type is above 4)
    cmp.l   %d1,%d0
    bcc.s   9f
    move.l  %d0,%d1
    move.l  %a1,%d0
    bsr.w   mfn_pg_page                  | a0 = the row (the descriptor)
    move.l  %a0,%d0
    move.l  (%sp)+,%d2
    rts
1:  move.l  %a1,%d0
    CLONEMAP 8
9:  jmp     dk_tail_763365db

| FUN(slot, type, sub) -> the parameter code (D2 saved by the stock code; a0 = the slot and d1 = 0x64 are loaded already; both are kept). Site (8 bytes):
| `move.l 0xc(sp),D0; movea.l 0x10(sp),A1`.
    .globl mf_typemap2
mf_typemap2:
    move.l  0xc(%sp),%d0
    cmp.l   MF_TAB+T_FIRST,%d0
    bcs.s   9f
    move.l  %a0,%d1
    movea.l %d0,%a1
    bsr.w   mfn_pg_slot                  | d0 = the code, d1 = 1 when the type has pages; a0 is lost
    movea.l 8(%sp),%a0
    tst.l   %d1
    beq.s   1f
    move.l  %a0,%d1
    subi.l  #0x19,%d1
    cmpi.l  #0x27,%d1
    bls.s   2f
    moveq   #0x64,%d1                   | not a machine slot: the stock code does not look at the type
    bra.s   9f
2:  move.l  (%sp)+,%d2
    rts
1:  move.l  %a1,%d0
    CLONEMAP 0xc
    moveq   #0x64,%d1                   | the stock code set d1 before the site and reads it again
9:  jmp     dk_tail_42d651c5

| ---- page rows. mfn_pg_count: d0 = type -> d0 = the number of its pages (builds the tables first); clobbers d1, a0.
mfn_pg_count:
    bsr.w   mfn_call_ensure
    move.l  %d2,-(%sp)
    move.l  MF_TAB+T_PGN,%d2
    movea.l MF_TAB+T_PGROWS,%a0
    moveq   #0,%d1
    tst.l   %d2
    beq.s   9f
1:  cmp.l   PG_MACH(%a0),%d0
    bne.s   2f
    addq.l  #1,%d1
2:  lea     PG_SIZE(%a0),%a0
    subq.l  #1,%d2
    bne.s   1b
9:  move.l  %d1,%d0
    move.l  (%sp)+,%d2
    rts

| mfn_pg_page: d0 = type, d1 = page (< its count) -> a0 = the page's row; clobbers d1.
mfn_pg_page:
    move.l  %d2,-(%sp)
    move.l  MF_TAB+T_PGN,%d2
    movea.l MF_TAB+T_PGROWS,%a0
    tst.l   %d2
    beq.s   8f
1:  cmp.l   PG_MACH(%a0),%d0
    bne.s   2f
    tst.l   %d1
    beq.s   7f
    subq.l  #1,%d1
2:  lea     PG_SIZE(%a0),%a0
    subq.l  #1,%d2
    bne.s   1b
8:  suba.l  %a0,%a0
7:  move.l  (%sp)+,%d2
    rts

| mfn_pg_slot: d0 = type, d1 = sound-array slot -> d0 = the code of the knob that has that slot on one of the type's pages (0 if none), d1 = 1 when the type has pages at all;
| clobbers a0.
mfn_pg_slot:
    lea     -12(%sp),%sp
    movem.l %d2-%d4,(%sp)
    move.l  MF_TAB+T_PGN,%d2
    movea.l MF_TAB+T_PGROWS,%a0
    moveq   #0,%d3
    moveq   #0,%d4
    tst.l   %d2
    beq.w   8f
1:  cmp.l   PG_MACH(%a0),%d0
    bne.w   6f
    moveq   #1,%d3
    .irp i,0,1,2,3,4,5,6,7,8
    cmp.l   PG_S + 4 * \i(%a0),%d1
    bne.s   5f
    move.l  PG_C + 4 * \i(%a0),%d4
    bra.w   8f
5:
    .endr
6:  lea     PG_SIZE(%a0),%a0
    subq.l  #1,%d2
    bne.w   1b
8:  move.l  %d4,%d0
    move.l  %d3,%d1
    movem.l (%sp),%d2-%d4
    lea     12(%sp),%sp
    rts

| ---- frame type: the frame builder (the vector-191 handler) writes the track's machine type into TX frame word 0x94 + 2t. The DSP's FM engine knows five types; a type above
| 4 is treated like MIDI (nothing rendered), so an added type is sent as its clone_of unless the entry has own_dsp (a registered voice routine renders it). Site (6 bytes):
| `move.b (0xd8c,A0),D0b; ext.w D0w`, a0 = the track; d0 is the result. An interrupt site: the image stub of the slot `frametype` enters here with a1 = this address (a1 and d0
| are free). The routine runs at the interrupt's priority: no call, no wait.
    .globl mf_frametype
mf_frametype:
    mvs.b   0xd8c(%a0),%d0              | the stock move.b and ext.w in one (the upper word of d0 is dead)
    cmp.l   MF_TAB+T_FIRST,%d0
    blt.s   9f
    lea     -12(%sp),%sp
    movem.l %d1-%d2/%a1,(%sp)
    move.l  %d0,%d1
    sub.l   MF_TAB+T_FIRST,%d1
    cmp.l   MF_TAB+T_COUNT,%d1
    bcc.s   1f
    move.l  #ROWSIZE,%d2
    muls.l  %d2,%d1
    add.l   MF_TAB+T_ROWS,%d1
    movea.l %d1,%a1
    move.l  O_OWN(%a1),%d2
    bne.s   1f
    move.l  O_CLONE(%a1),%d0
1:  movem.l (%sp),%d1-%d2/%a1
    lea     12(%sp),%sp
9:  movea.l MF_TAB+T_R_FT,%a1
    jmp     (%a1)

| ---- type reader: `FUN_4004b7f2` returns the machine type of a sound (`mvs.b (0xde,A0),D0`, then a bra.b to the epilogue). Its callers are pages and parameter code that test
| the type against FM TONE's and so on, so a clone must look like its original to them. The identity code (list stepping, the name of the machine, sound snapshots, preset and
| clear: nine call sites) wants the entry's own type: the routine compares the return address of the reader's caller (4(sp): the reader pushed one register) with the end of
| each of those calls (the table words RC0 .. RC8). Site (6 bytes): the load and the bra.b; the routine ends at the epilogue (site + 8, RD).
    .globl mf_reader
mf_reader:
    mvs.b   0xde(%a0),%d0
    cmp.l   MF_TAB+T_FIRST,%d0
    blt.w   9f
    move.l  %d0,%d1
    sub.l   MF_TAB+T_FIRST,%d1
    cmp.l   MF_TAB+T_COUNT,%d1
    bcc.w   9f
    move.l  4(%sp),%d1
    .irp i,0,1,2,3,4,5,6,7,8
    cmp.l   MF_TAB+T_RC0 + 4 * \i,%d1
    beq.w   9f
    .endr
    | a machine with own pages is its own type for every caller (the page, draw, defaults and clipboard code all index per-type structures that have its rows): the other added
    | types look like their clone
    move.l  %d0,-(%sp)
    bsr.w   mfn_pg_count
    move.l  %d0,%d1
    move.l  (%sp)+,%d0
    tst.l   %d1
    bne.w   9f
    move.l  %a1,-(%sp)
    sub.l   MF_TAB+T_FIRST,%d0
    move.l  #ROWSIZE,%d1
    muls.l  %d1,%d0
    add.l   MF_TAB+T_ROWS,%d0
    movea.l %d0,%a1
    move.l  O_CLONE(%a1),%d0
    movea.l (%sp)+,%a1
9:  move.l  MF_TAB+T_R_RD,-(%sp)
    rts

| ---- owner test of `SoundParameterSet::vfunc_21` (type == the parameter's owner machine): a clone looks like its original. Site (6 bytes): `mvs.b (0xde,A0),D3; move.l D2,-(SP)`.
| d0 and a1 are saved.
    .globl mf_owner
mf_owner:
    mvs.b   0xde(%a0),%d3
    cmp.l   MF_TAB+T_FIRST,%d3
    blt.s   9f
    lea     -8(%sp),%sp
    movem.l %d0/%a1,(%sp)
    move.l  %d3,%d0
    sub.l   MF_TAB+T_FIRST,%d0
    cmp.l   MF_TAB+T_COUNT,%d0
    bcc.s   1f
    move.l  #ROWSIZE,%d3
    muls.l  %d3,%d0
    add.l   MF_TAB+T_ROWS,%d0
    movea.l %d0,%a1
    move.l  O_CLONE(%a1),%d3
    bra.s   2f
1:  move.l  %d0,%d3
    add.l   MF_TAB+T_FIRST,%d3
2:  movem.l (%sp),%d0/%a1
    lea     8(%sp),%sp
9:  move.l  %d2,-(%sp)                  | the second displaced instruction
    move.l  MF_TAB+T_R_OW,-(%sp)
    rts
