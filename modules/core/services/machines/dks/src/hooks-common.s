| machines.dks, the hooks both devices share: the machine page's draw, code, key and turn entries. Each continues in the stock code through the tail `patchkit bind` makes
| (`dk_tail_<hash of the hook>`), so the routines hold no firmware bytes and one routine serves both devices. `MF_TAB` is the image table `machines_info` (`image.rs`).

    .text
    .balign 4

    .equ T_N_CODE,    52
    .equ T_N_DRAW,    56
    .equ T_N_OVERLAY, 60

| ---- the machine page's draw, code, key and turn entries (SourcePageView and MultiSourcePageView, vfunc_4, vfunc_47, vfunc_2, vfunc_17: the same code on both devices; `patchkit bind` makes the stock tail of each hook for the firmware it binds to). The stock path is the tail `patchkit bind` makes. At the entry 0(sp) = the return address, 4(sp) = the view, 8(sp) = the event (or canvas); d0/d1/a0/a1 are free.
| The native page seams are the adapter's C functions (`mf_native_*` of the image, table words N_*); -1 from code means pass through; 0 is an intentional unused/LOCAL
| mapping. `dks_page_key` and `dks_page_turn` are Rust (lib.rs).
mf_draw_stock:                          | the stock function from its first instruction: called, so the stock `rts` returns here
    jmp     dk_tail_04bb3ae8

    .globl mf_draw_hook
mf_draw_hook:
    move.l  8(%sp),-(%sp)
    move.l  8(%sp),-(%sp)
    movea.l MF_TAB+T_N_DRAW,%a0
    jsr     (%a0)
    addq.l  #8,%sp
    tst.l   %d0
    beq.s   1f
    cmpi.l  #2,%d0
    beq.s   2f
    rts
1:  jmp     dk_tail_04bb3ae8
2:  move.l  8(%sp),-(%sp)               | original stack: return, view, canvas; each callee sees return, view, canvas
    move.l  8(%sp),-(%sp)
    bsr.w   mf_draw_stock
    addq.l  #8,%sp
    move.l  8(%sp),-(%sp)
    move.l  8(%sp),-(%sp)
    movea.l MF_TAB+T_N_OVERLAY,%a0
    jsr     (%a0)
    addq.l  #8,%sp
    rts

    .globl mf_code_hook
mf_code_hook:
    move.l  8(%sp),-(%sp)
    move.l  8(%sp),-(%sp)
    movea.l MF_TAB+T_N_CODE,%a0
    jsr     (%a0)
    addq.l  #8,%sp
    cmpi.l  #-1,%d0
    beq.s   1f
    rts
1:  jmp     dk_tail_1e264a23

    .globl mf_key_hook
mf_key_hook:
    move.l  8(%sp),-(%sp)       /* event */
    move.l  8(%sp),-(%sp)       /* view: 4(sp) at the entry, 8 after one push */
    jsr     dks_page_key
    addq.l  #8,%sp
    jmp     dk_tail_6113bd45

    .globl mf_turn_hook
mf_turn_hook:
    move.l  8(%sp),-(%sp)
    move.l  8(%sp),-(%sp)
    jsr     dks_page_turn
    addq.l  #8,%sp
    tst.b   %d0
    beq.s   1f
    moveq   #1,%d0                      /* the native dispatcher consumes a nonzero D0b */
    rts
1:  jmp     dk_tail_d46e3075
