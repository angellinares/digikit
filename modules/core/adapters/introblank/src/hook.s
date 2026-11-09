| core/introblank: the hook before the intro setup function reads its scene table argument (cf.rtos.hook.intro_setup.table_arg). ColdFire, gas, mcpu=54418.
|
| Entered by `jmp` from the setup function, after `lea -0x1c,sp; clr.l d1; move.l d2,-(sp)`: the argument is at 0x24(sp). The hook fills the adapter's
| scene table, writes its address over the argument and continues through the hook tail, where the displaced `move.l 0x24(sp),d0; movea.l d0,a0` run.
| d0 and a0 are used: the displaced `move.l 0x24(sp),d0; movea.l d0,a0` load both next. The condition codes are dead too.
    .text
    .even
    .globl introblank_hook
introblank_hook:
    lea     introblank_table,%a0
    move.l  #introblank_black,%d0
    move.l  %d0,(%a0)
    move.l  #175,%d0
    move.l  %d0,4(%a0)
    clr.l   8(%a0)
    clr.l   12(%a0)
    move.l  %a0,0x24(%sp)
    jmp     introblank_next
