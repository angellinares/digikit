| core/machines on the Digitakt II (1.16, 1.17): the ColdFire routines the hooks of module.toml enter.
|
| The framework does not know the machines at build time. A content module adds an `[[entry]] registry =
| "machines"`; patchkit lays the entries out as the table `machines.rows` (row layout in module.toml, offsets O_*
| below), gives every entry its permanent type number (types.toml) and copies the stock tables (names, permission masks,
| per-type bytes, DSP selector) with one row appended per entry. These routines look the rows up at run time for a
| type >= machines.first, and give every other type to the stock code unchanged.
|
| Hook convention (patchkit "hook" op, displaced = "run"): the site is overwritten with `jmp.l <routine>`; a routine
| that does not take the type over ends with `jmp <hook tail>` (the displaced instructions, then the rest of the stock
| code). A routine may use only the registers its site frees (stated per routine); everything else it saves.
| Registers named in a comment as "free" are scratch at that point of the stock code (read from the disassembly).
|
| Lazily built tables (mf_ensure, once, the first time a routine needs them): the displayed order of the machine
| list (stock order with every entry inserted at its `position`; gap rows, position -1, are not listed), the rank pairs the sort comparator's std::map
| takes, and the std::string representations the dispatch descriptors point to (upper-cased name and short name).

    .text

| row layout (bytes), the field order of module.toml's [[registry.field]]
    .equ O_LONG,   0           | descriptor: chars of the std::string long name (built by mf_ensure)
    .equ O_SHORT,  4           | descriptor: chars of the std::string short name (built by mf_ensure)
    .equ O_F0,     8           | descriptor: the nine parameter descriptor IDs, O_F0 .. O_F0 + 32
    .equ O_CLONE,  44
    .equ O_POS,    48
    .equ O_OWN,    52          | Digitone II: the DSP is told the entry's own type (unused here)
    .equ O_NAME,   56          | pointer to the display name (C string)
    .equ O_SNAME,  60          | pointer to the short label (C string)
    .equ ROWSIZE,  64
    .equ MAXN,     32          | registry max
    .equ REPSIZE,  48          | bytes per std::string representation in mf_strbuf

| ---- mf_ensure: build the run-time tables once. Preserves every register (not the flags).
mf_ensure:
    tst.b   mf_ready
    bne.w   9f
    lea     -60(%sp),%sp
    movem.l %d0-%d7/%a0-%a6,(%sp)
    | list: the stock order, then each entry inserted at min(position, length)
    lea     mf_list_buf,%a0
    lea     _mf_list_stock,%a1
    move.l  #_mf_first,%d2
    move.l  %d2,%d3                 | d3 = length
1:  move.l  (%a1)+,(%a0)+
    subq.l  #1,%d2
    bne.s   1b
    lea     _mf_rows,%a2
    move.l  #_mf_count,%d4
    move.l  #_mf_first,%d5          | d5 = the type of entry k
    tst.l   %d4
    beq.s   5f
2:  move.l  O_POS(%a2),%d0
    addq.l  #1,%d0
    beq.s   20f                     | position -1: a gap row (a number nobody selected): not listed
    subq.l  #1,%d0
    cmp.l   %d3,%d0
    bls.s   3f
    move.l  %d3,%d0
3:  lea     mf_list_buf,%a0
    move.l  %d3,%d1
4:  cmp.l   %d0,%d1                 | shift the entries from position up by one
    bls.s   6f
    move.l  -4(%a0,%d1.l*4),%d6
    move.l  %d6,(%a0,%d1.l*4)
    subq.l  #1,%d1
    bra.s   4b
6:  move.l  %d5,(%a0,%d0.l*4)
    addq.l  #1,%d3
20: addq.l  #1,%d5
    lea     ROWSIZE(%a2),%a2
    subq.l  #1,%d4
    bne.s   2b
5:  | rank pairs {type, display position}
    lea     mf_list_buf,%a0
    lea     mf_rank_buf,%a1
    moveq   #0,%d0
7:  move.l  (%a0)+,(%a1)+
    move.l  %d0,(%a1)+
    addq.l  #1,%d0
    cmp.l   %d3,%d0
    bcs.s   7b
    | descriptors: std::string representations of the upper-cased names
    lea     _mf_rows,%a2
    lea     mf_strbuf,%a3
    move.l  #_mf_count,%d4
    tst.l   %d4
    beq.s   8f
10: movea.l O_NAME(%a2),%a0
    bsr.s   mf_rep
    move.l  %d0,O_LONG(%a2)
    movea.l O_SNAME(%a2),%a0
    bsr.s   mf_rep
    move.l  %d0,O_SHORT(%a2)
    lea     ROWSIZE(%a2),%a2
    subq.l  #1,%d4
    bne.s   10b
8:  moveq   #-1,%d0
    move.b  %d0,mf_ready
    movem.l (%sp),%d0-%d7/%a0-%a6
    lea     60(%sp),%sp
9:  rts

| a0 = C string, a3 = destination; builds {length, capacity, refcount -1, upper-cased chars, 0} at a3, returns the
| address of the chars in d0 and advances a3 to the next 4-byte boundary. Clobbers d1, d2, a0, a1.
mf_rep:
    moveq   #0,%d1
    lea     12(%a3),%a1
1:  move.b  (%a0)+,%d2
    beq.s   2f
    cmpi.b  #'a',%d2
    blo.s   3f
    cmpi.b  #'z',%d2
    bhi.s   3f
    subi.l  #32,%d2
3:  move.b  %d2,(%a1)+
    addq.l  #1,%d1
    bra.s   1b
2:  clr.b   (%a1)
    addq.l  #1,%a1
    move.l  %d1,(%a3)
    move.l  %d1,4(%a3)
    moveq   #-1,%d2
    move.l  %d2,8(%a3)
    move.l  %a3,%d0
    addi.l  #12,%d0
    move.l  %a1,%d2
    addq.l  #3,%d2
    andi.l  #-4,%d2
    movea.l %d2,%a3
    rts

| ---- dispatch: `FUN_xxxx(type)` returns the descriptor of a machine type. Site: function entry (d0, d1 free).
    .globl mf_dispatch
mf_dispatch:
    move.l  4(%sp),%d0
    subi.l  #_mf_first,%d0
    cmpi.l  #_mf_count,%d0
    bcc.s   1f
    bsr.w   mf_ensure
    move.l  #ROWSIZE,%d1
    muls.l  %d1,%d0
    addi.l  #_mf_rows,%d0
    rts
1:  jmp     _mf_t_dispatch

| ---- list builder entry: make sure the tables exist before the builder reads mf_list_buf (the builder's literals
| point there). Site: function entry; every register is preserved.
    .globl mf_list_entry
mf_list_entry:
    bsr.w   mf_ensure
    jmp     _mf_t_list

| ---- rank: the std::map insert gets the (type, position) pairs of mf_rank_buf instead of the caller's array.
| Site: the `jsr` of the insert; stack: 0 map, 4 begin, 8 end. d0 free.
    .globl mf_rank
mf_rank:
    bsr.w   mf_ensure
    move.l  #mf_rank_buf,%d0
    move.l  %d0,4(%sp)
    move.l  #_mf_first,%d0
    addi.l  #_mf_entries,%d0        | the listed machines (gap rows are not in mf_rank_buf)
    lsl.l   #3,%d0
    addi.l  #mf_rank_buf,%d0
    move.l  %d0,8(%sp)
    jmp     _mf_t_rank

| descriptor field offset of mirror index 25 .. 34 (-1: none)
    .section .rodata
    .balign 4
    .globl mf_slotmap
mf_slotmap:
    .long   8, 12, 16, 20, -1, -1, 24, 28, 32, 36

    .bss
    .balign 4
    .globl mf_list_buf, mf_rank_buf
mf_ready:    .space 4
mf_list_buf: .space 4 * 64
mf_rank_buf: .space 8 * 64
mf_strbuf:   .space REPSIZE * 2 * MAXN

    .text
    .globl mf_native_type
mf_native_type:
    jmp     _mf_spt

| ---- the table `machines_info` (an [[info]] of module.toml): what the service file `machines.dks` needs of the image and the firmware (modules/core/services/machines/dks/src/
| image.rs has the layout; the word index is the byte offset / 4). Addresses only: the file's hooks continue in the stock code through the tails `patchkit bind` makes, or
| through the exits below. 1 = the Digitakt II.
    .section .rodata
    .balign 4
    .globl machines_info
machines_info:
    .long   0x4d414348, 1, 1                | magic MACH, version, device
    .long   _mf_k_keydown, _mf_k_setvalue, _mf_k_viewtrack
    .long   _mf_first, _mf_count, _mf_entries, _mf_rows
    .long   mf_ensure
    .long   mf_native_page_count, mf_native_descriptor, mf_native_code, mf_native_draw, mf_native_overlay
    .long   _mf_spt, mf_slotmap, 0, 0       | current track type, the slot map, (page rows, page count: Digitone II)
    .long   _mf_c5 + 12, _mf_c5a            | clone.5: not equal, equal
    .long   _mf_x_typearg                   | the second exit of the parameter code
    .long   _mf_sp + 8, _mf_spa             | SRC page: the stock path, the knob-page branch
    .long   0, 0                            | group (Digitone II)
    .long   0, 0, 0                         | frame type, type reader, owner test (Digitone II)
    .space  4 * 9                           | the calls of the type reader (Digitone II)
