| machines.dks on the Digitakt II (1.17): the ColdFire routines the service's hooks run. They were in core/machines (`src/cf/dt2/machines.s`).
|
| Each hook is entered by a `jmp.l` that the runtime writes at its firmware site (`patchkit bind` made the hook raw: no trampoline). The routine takes the whole
| control flow of the site and ends in the stock code either by `jmp dk_tail_<hash of the hook>` (the displaced instructions of the site as in the user's firmware, made by
| `patchkit bind`) or, when a branch is among them, by pushing an exit address of the table `MF_TAB` and doing `rts` (the stack is as it was at the site, so the pair changes no
| register or flag).
| `MF_TAB` is the image table `machines_info` of core/machines, copied by `setup` (lib.rs); its words give the registry rows and counts, the addresses of the
| firmware and of the adapter's routines, and where each hook continues. Offsets T_* below; the same layout is in hooks-dn2.s and in `image.rs`.
|
| Hook convention: registers the stock site frees are scratch (`devices/fw/machine.toml` lists them per hook); everything else a routine saves.
| The image keeps `mf_ensure` (the tables of the registry are built once, on first use), the machine list and rank hooks, and the dispatch hook: it is the one
| routine `mf_ensure` is called through (`mf_call_ensure`).

    .text
    .balign 4

| row layout (bytes), the field order of core/machines' [[registry.field]]
    .equ O_CLONE,  44
    .equ ROWSIZE,  64


| the table (byte offsets of the words of MF_TAB; `image.rs` has the same layout)
    .equ T_FIRST, 24
    .equ T_COUNT, 28
    .equ T_ROWS, 36
    .equ T_N_PAGE_COUNT, 44
    .equ T_N_CODE, 52
    .equ T_N_DRAW, 56
    .equ T_N_OVERLAY, 60
    .equ T_SPT, 64
    .equ T_SLOTMAP, 68
    .equ T_R_C5_NE, 80
    .equ T_R_C5_EQ, 84
    .equ T_X_TYPEARG, 88
    .equ T_R_SP_STOCK, 92
    .equ T_R_SP_ALT, 96

| mfd_row: d0 = index k (< count) -> a0 = &machines.rows[k]. Preserves every other register (not the flags).
mfd_row:
    move.l  %d1,-(%sp)
    move.l  #ROWSIZE,%d1
    muls.l  %d0,%d1
    add.l   MF_TAB+T_ROWS,%d1
    movea.l %d1,%a0
    move.l  (%sp)+,%d1
    rts

| clone.5 is the masked test `(type & ~2) == 4` (types 4 and 6): d0 becomes the clone_of of an added type before the stock mask. Site (12 bytes): the test itself,
| `moveq #-3,D1; and.l D1,D0; moveq #4,D1; cmp.l D0,D1; beq.w T`. It continues at T (the table word EQ) when equal, else behind the site.
    .globl mf_clone5
mf_clone5:
    move.l  %a0,-(%sp)
    move.l  %d0,%d1
    sub.l   MF_TAB+T_FIRST,%d0
    cmp.l   MF_TAB+T_COUNT,%d0
    bcc.s   1f
    bsr.w   mfd_row
    move.l  O_CLONE(%a0),%d1
1:  move.l  %d1,%d0
    movea.l (%sp)+,%a0
    moveq   #-3,%d1                     | the stock test
    and.l   %d1,%d0
    moveq   #4,%d1
    cmp.l   %d0,%d1
    bne.s   2f
    move.l  MF_TAB+T_R_C5_EQ,-(%sp)
    rts
2:  move.l  MF_TAB+T_R_C5_NE,-(%sp)
    rts

| ---- type argument of the parameter mapping: `FUN(index, type, filter)` answers the parameter code of a (mirror index 25..34, machine type) pair. For an added type:
| the slots with a descriptor field answer with the entry's field, the two without (29, 30: MANUAL SLICE's Sample Bank / Slot rows, which the stock map holds for its
| owner but no descriptor lists) answer as the clone does (the stack argument is replaced by the clone's type, then the stock code runs); every other index runs the
| stock code. Site (8 bytes): `move.l 0x10(sp),D0; movea.l 0x14(sp),A1` in the middle of the function; a0 = index, d1 live; d0, a1 are reloaded, d2 and a2 are saved by
| the function (they are saved here too).
    .globl mf_typearg
mf_typearg:
    move.l  0x10(%sp),%d0
    sub.l   MF_TAB+T_FIRST,%d0
    cmp.l   MF_TAB+T_COUNT,%d0
    bcc.s   9f
    lea     -16(%sp),%sp
    movem.l %d1-%d2/%a0/%a2,(%sp)
    move.l  %a0,%d2
    subi.l  #25,%d2
    cmpi.l  #9,%d2
    bhi.s   7f
    lsl.l   #2,%d2
    movea.l MF_TAB+T_SLOTMAP,%a2
    move.l  (%a2,%d2.l),%d2
    bmi.s   6f
    bsr.w   mfd_row
    move.l  (%a0,%d2.l),%d0
    movem.l (%sp),%d1-%d2/%a0/%a2
    lea     16(%sp),%sp
    move.l  MF_TAB+T_X_TYPEARG,-(%sp)   | the second exit of the stock code, with d0 = the field
    rts
6:  bsr.w   mfd_row
    move.l  O_CLONE(%a0),%d0
    movem.l (%sp),%d1-%d2/%a0/%a2
    lea     16(%sp),%sp
    move.l  %d0,0x10(%sp)
    bra.s   9f
7:  movem.l (%sp),%d1-%d2/%a0/%a2
    lea     16(%sp),%sp
9:  jmp     dk_tail_09514a38            | the stock instructions of the site, made by `patchkit bind`, and on

| ---- SRC page count (docs/findings/50). The page list of a `ParameterPageView` is a vector of 4-byte entries at view+0x7c (begin) and view+0x80 (end); the current page is
| view+0x90. `SourcePageView` has two entries (the knob page and the sample view). An added machine has ONE SRC page: at the start of `SourcePageView::vfunc_4` the vector is cut
| to SRC_PAGES entries and the page index is 0 when the track's type is an added one, and a stock type gets its two entries back. Site (8 bytes, the first test of the
| function): `tst.l 0x90(A2); beq.w T`; a2 = the view, d2 = the draw context. T (table word SP_ALT) is the knob-page branch; the stock path goes on at site + 8
| (SP_STOCK). SPT is `FUN_4005f6bc` (the type as a signed long, -1 without a track). d0, d1, a0, a1 are free at the site.
    .equ    SRC_PAGES,  1
    .equ    SRC_STOCK,  2           | entries of the stock vector
    .globl mf_srcpage
mf_srcpage:
    move.l  %a2,-(%sp)
    movea.l MF_TAB+T_SPT,%a1
    jsr     (%a1)
    addq.l  #4,%sp
    cmp.l   MF_TAB+T_FIRST,%d0
    blt.s   1f
    move.l  #SRC_PAGES,-(%sp)
    move.l  %d0,-(%sp)
    movea.l MF_TAB+T_N_PAGE_COUNT,%a1
    jsr     (%a1)
    addq.l  #8,%sp
    move.l  %d0,%d1
    lsl.l   #2,%d1
    add.l   0x7c(%a2),%d1
    move.l  %d1,0x80(%a2)            | existing capacity is two entries; no allocation or vector growth
    cmp.l   0x90(%a2),%d0
    bhi.s   4f
    clr.l   0x90(%a2)
4:  move.l  MF_TAB+T_R_SP_ALT,-(%sp)
    rts
1:  move.l  0x80(%a2),%d0           | any other type: the stock vector again if an added type cut it
    sub.l   0x7c(%a2),%d0
    subq.l  #4 * SRC_PAGES,%d0
    bne.s   2f
    move.l  0x7c(%a2),%d1
    addi.l  #4 * SRC_STOCK,%d1
    move.l  %d1,0x80(%a2)
2:  tst.l   0x90(%a2)
    beq.w   3f
    move.l  MF_TAB+T_R_SP_STOCK,-(%sp)
    rts
3:  move.l  MF_TAB+T_R_SP_ALT,-(%sp)
    rts
