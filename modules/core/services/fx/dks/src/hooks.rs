//! The nine hooks of the effect pages, in assembler (ColdFire, GNU syntax). They were `core/fxpages`' `fxpages-dt2.s` and `fxpages-dn2.s`; the two were the same code
//! with other numbers. Here the numbers are read when the hook runs, from `DKRT_FXP_TAB` (the service binds it from the image table `fxpages_info`, `fxpage::setup`), so one
//! file serves both devices. Design: docs/design/digikit-registry.md ("Effect pages"), docs/findings/61-fx-pages.md.
//!
//! Each hook runs in place of the first whole instructions of its stock site (6 bytes, or 8 for two of them) and ends in the stock code behind them: the displaced
//! instructions are redone by the hook, and the return address is a word of the table (the site plus its length, or another place of the stock routine). Registers:
//! the hook may clobber what `devices/fw/page.toml` lists for it. A hook that returns into code that tests the flags (`fxp_label`) jumps through `a0`
//! (`movea` and `jmp` leave the flags); the others push the address and `rts` (the stack is as it was at the site).
//!
//! - `fxp_acc`: the page descriptor accessor `row = f(int page)`. A page number `first + k` (k below the effect slots) answers with row k of `DKRT_FXP_ROWS`; the number
//!   `first + slots` answers with the send row; every other number runs the stock code.
//! - `fxp_list`: the FX view builder pushes its page count and loads the address of its list of page numbers: with `DKRT_FXP_COUNT` not 0 the count on the stack is
//!   replaced and the list is `DKRT_FXP_LIST_BUF`.
//! - `fxp_keylist`: the same for the track FX key's list (`DKRT_FXP_SEND_COUNT`, `DKRT_FXP_KEY_LIST`): the second page is the send page.
//! - `fxp_label`: the tab label of the FX view's page: an effect page pushes the name of its row and runs the tail of the label code.
//! - `fxp_graph`: the FX view skips the filter graph over the knobs 5 and 6 on page 15; the effect pages skip it too.
//! - `fxp_sound`: the end of the sound loader counts the loads (`DKRT_FXP_NSOUND`).
//! - `fxp_load`, `fxp_save`, `fxp_init`: the kit's effect values (finding 61): the loader gives each row with an effect the stored values (or the defaults when the stored
//!   effect ID differs, or a value is out of range), the saver writes the values and the IDs into the spare halfwords of the storage record, the initialiser gives
//!   the defaults. The loader and the initialiser put the mark (`MARKVAL` at `MARK_OFF` in the array) when a row has an effect.
//!
//! Table offsets (bytes): the image table `fxpages_info` version 3 (`core/fxpages`) and the words `fxpage::bind_table` adds (from `ROWS_N`).
#[cfg(target_arch = "m68k")]
use core::arch::global_asm;

macro_rules! entries {
    ($($name:ident,)*) => {
        $(
            #[cfg(target_arch = "m68k")]
            extern "C" {
                /// An entry (a label of the assembler below): the runtime writes a jump to it at its stock site.
                pub fn $name();
            }
            // a host build (the assembly test of the runtime runs the declarations of `service!`) has an empty function, so that the entry address exists
            #[cfg(not(target_arch = "m68k"))]
            pub extern "C" fn $name() {}
        )*
    };
}

entries! {
    fxp_acc,
    fxp_label,
    fxp_graph,
    fxp_list,
    fxp_load,
    fxp_save,
    fxp_init,
    fxp_keylist,
    fxp_sound,
}

#[cfg(target_arch = "m68k")]
global_asm!(
    r#"
    .equ ROW_SIZE, 0x2c         | the firmware's page descriptor row: name, title (char pointers), 9 codes

    .equ T_FIRST, 20            | the page number of effect row 0
    .equ T_SLOTS, 24            | effect rows
    .equ T_STOCK, 40            | the stock list of page numbers
    .equ T_VIDX, 80             | the array index of each value (bytes)
    .equ T_MARKVAL, 152         | the mark's value
    .equ T_R_ACC, 196           | the stock code behind each hook's site
    .equ T_R_LIST, 200
    .equ T_R_KEYLIST, 204
    .equ T_R_SOUND, 208
    .equ T_R_LABEL_TAIL, 212    | the tail of the label code (pushes the position and draws)
    .equ T_R_LABEL, 216
    .equ T_R_GRAPH_SKIP, 220    | the target of the stock `beq.w` that skips the graph
    .equ T_R_GRAPH, 224
    .equ T_R_LOAD, 228
    .equ T_R_SAVE, 232
    .equ T_R_INIT, 236
    .equ T_KEYSTOCK, 240        | the stock list of the track FX key
    .equ T_TRACKPAGE, 244       | the track FX page number
    .equ T_TAIL, 248            | byte offset of the last word of the array that stock moves with the storage record
    .equ T_STORE_OFF, 252       | byte offset of the spare halfwords in the storage record
    .equ T_ROWS_N, 256          | effect rows with values in the kit (8 values each)
    .equ T_ID_OFF, 260          | byte offset of the first effect ID in the storage record
    .equ T_MARK_OFF, 264        | byte offset of the mark in the array

    .text
    .balign 4

    .globl fxp_acc
fxp_acc:
    move.l  4(%sp),%d0
    sub.l   DKRT_FXP_TAB+T_FIRST,%d0
    move.l  DKRT_FXP_TAB+T_SLOTS,%d1
    addq.l  #1,%d1
    cmp.l   %d1,%d0
    bcc.s   1f                          | not one of our pages (also a number below the first)
    subq.l  #1,%d1
    cmp.l   %d1,%d0
    beq.s   2f                          | the send page
    moveq   #ROW_SIZE,%d1
    muls.l  %d1,%d0
    addi.l  #DKRT_FXP_ROWS,%d0
    rts
1:  move.l  4(%sp),%d0                  | the displaced instructions (the stock bound: the pages below the first)
    move.l  DKRT_FXP_TAB+T_FIRST,%d1
    subq.l  #1,%d1
    movea.l DKRT_FXP_TAB+T_R_ACC,%a0
    jmp     (%a0)
2:  move.l  DKRT_FXP_TAB+T_TRACKPAGE,-(%sp)  | the send page: the track FX page's name, title and level slot, from the stock accessor
    jsr     fxp_stock_acc
    addq.l  #4,%sp
    movea.l %d0,%a0
    lea     DKRT_FXP_SEND_ROW,%a1
    move.l  (%a0),(%a1)                 | name
    move.l  4(%a0),4(%a1)               | title
    move.l  8+4*8(%a0),8+4*8(%a1)       | the level slot (the ninth code)
    move.l  %a1,%d0
    rts

fxp_stock_acc:                          | the stock accessor, called as a function: the page number at 4(SP)
    move.l  4(%sp),%d0
    move.l  DKRT_FXP_TAB+T_FIRST,%d1
    subq.l  #1,%d1
    movea.l DKRT_FXP_TAB+T_R_ACC,%a0
    jmp     (%a0)

    .globl fxp_list
fxp_list:
    move.l  DKRT_FXP_COUNT,%d0
    beq.s   1f
    move.l  %d0,(%sp)                   | the count the builder pushed (the stock 3)
    move.l  #DKRT_FXP_LIST_BUF,%d0
    move.l  DKRT_FXP_TAB+T_R_LIST,-(%sp)
    rts
1:  move.l  DKRT_FXP_TAB+T_STOCK,%d0         | the displaced instruction
    move.l  DKRT_FXP_TAB+T_R_LIST,-(%sp)
    rts

    .globl fxp_keylist
fxp_keylist:
    move.l  DKRT_FXP_SEND_COUNT,%d0
    beq.s   1f
    move.l  %d0,(%sp)                   | the count the builder pushed (the stock 1)
    move.l  #DKRT_FXP_KEY_LIST,%d0
    move.l  DKRT_FXP_TAB+T_R_KEYLIST,-(%sp)
    rts
1:  move.l  DKRT_FXP_TAB+T_KEYSTOCK,%d0      | the displaced instruction
    move.l  DKRT_FXP_TAB+T_R_KEYLIST,-(%sp)
    rts

    .globl fxp_sound
fxp_sound:
    addq.l  #1,DKRT_FXP_NSOUND               | D0 (the loader's result) is not touched
    movem.l (%sp),%d2-%d4/%a2-%a6       | the displaced instructions
    lea     0x20(%sp),%sp
    move.l  DKRT_FXP_TAB+T_R_SOUND,-(%sp)
    rts

    .globl fxp_label
fxp_label:
    move.l  %d0,%d1
    sub.l   DKRT_FXP_TAB+T_FIRST,%d1
    cmp.l   DKRT_FXP_TAB+T_SLOTS,%d1
    bcc.s   1f
    moveq   #ROW_SIZE,%d0
    muls.l  %d0,%d1
    addi.l  #DKRT_FXP_ROWS,%d1
    movea.l %d1,%a0
    move.l  (%a0),-(%sp)                | the tab label, as the stock code pushes its string
    movea.l DKRT_FXP_TAB+T_R_LABEL_TAIL,%a0
    jmp     (%a0)
1:  movea.l DKRT_FXP_TAB+T_STOCK,%a0         | the displaced instructions: the first page of the stock list (DEL; D0 is untouched)
    move.l  (%a0),%d1
    cmp.l   %d0,%d1
    movea.l DKRT_FXP_TAB+T_R_LABEL,%a0       | movea and jmp keep the flags of the compare for the stock `bne.b`
    jmp     (%a0)

    .globl fxp_graph
fxp_graph:
    cmp.l   %d0,%d5                     | the stock compare: page 15
    beq.s   1f
    cmp.l   DKRT_FXP_TAB+T_FIRST,%d0
    bcs.s   2f
1:  move.l  DKRT_FXP_TAB+T_R_GRAPH_SKIP,-(%sp)   | no graph
    rts
2:  move.l  DKRT_FXP_TAB+T_R_GRAPH,-(%sp)
    rts

    .globl fxp_load
fxp_load:
    move.l  0x98(%a0),%d0               | the displaced instruction (A0 = storage record, A1 = the array)
    move.l  DKRT_FXP_TAB+T_TAIL,%d1
    move.l  %d0,(0,%a1,%d1.l)
    lea     -20(%sp),%sp                | ColdFire movem has no predecrement form
    movem.l %d2-%d4/%a4-%a5,(%sp)
    move.l  %a1,DKRT_FXP_LAST_LOAD
    move.l  %a0,DKRT_FXP_LOAD_SRC
    addq.l  #1,DKRT_FXP_NLOAD
    move.l  DKRT_FXP_TAB+T_ID_OFF,%d2
    lea     (0,%a0,%d2.l),%a3           | the IDs the kit stored
    move.l  DKRT_FXP_TAB+T_STORE_OFF,%d2
    lea     (0,%a0,%d2.l),%a0           | the values it stored
    lea     DKRT_FXP_IDS,%a2
    lea     DKRT_FXP_DEFS,%a4
    lea     DKRT_FXP_TAB+T_VIDX,%a5
    move.l  DKRT_FXP_TAB+T_ROWS_N,%d0
1:  mvz.w   (%a2)+,%d1                  | the effect in this row now (0: none)
    mvz.w   (%a3)+,%d2                  | the effect the kit has stored here
    tst.l   %d1
    bne.s   2f
    lea     16(%a0),%a0                 | no effect: leave the row (the stock loader zeroed it)
    lea     16(%a4),%a4
    lea     8(%a5),%a5
    bra.s   7f
2:  move.l  DKRT_FXP_TAB+T_MARK_OFF,%d3      | an effect is here: the array holds its row (stored values or defaults) from now on
    move.l  DKRT_FXP_TAB+T_MARKVAL,%d4
    move.w  %d4,(0,%a1,%d3.l)
    moveq   #0,%d3                      | d3 = 1: the stored values are this effect's
    cmp.l   %d1,%d2
    bne.s   3f
    moveq   #1,%d3
3:  moveq   #8,%d4
4:  mvz.w   (%a0)+,%d1                  | the stored value
    mvz.w   (%a4)+,%d2                  | the default
    tst.l   %d3
    bne.s   5f
    move.l  %d2,%d1                     | another effect (or none) was here: the default
5:  cmpi.l  #0x7f00,%d1                 | a raw value is 0 .. 0x7f00
    bls.s   6f
    move.l  %d2,%d1
6:  mvz.b   (%a5)+,%d2                  | the array index of this value
    move.w  %d1,(0,%a1,%d2.l*2)
    subq.l  #1,%d4
    bne.s   4b
7:  subq.l  #1,%d0
    bne.s   1b
    movem.l (%sp),%d2-%d4/%a4-%a5
    lea     20(%sp),%sp
    movea.l DKRT_FXP_TAB+T_R_LOAD,%a0
    jmp     (%a0)

    .globl fxp_save
fxp_save:
    move.l  DKRT_FXP_TAB+T_TAIL,%d0          | the displaced instruction (A2 = storage record, A3 = the array)
    move.l  (0,%a3,%d0.l),%d1
    move.l  %d1,0x98(%a2)
    lea     -20(%sp),%sp
    movem.l %d2-%d4/%a4-%a5,(%sp)
    move.l  %a3,DKRT_FXP_LAST_SAVE
    move.l  %a2,DKRT_FXP_SAVE_DST
    addq.l  #1,DKRT_FXP_NSAVE
    move.l  DKRT_FXP_TAB+T_ID_OFF,%d2
    lea     (0,%a2,%d2.l),%a1           | where the IDs go
    move.l  DKRT_FXP_TAB+T_STORE_OFF,%d2
    lea     (0,%a2,%d2.l),%a0           | where the values go
    lea     DKRT_FXP_IDS,%a4
    lea     DKRT_FXP_TAB+T_VIDX,%a5
    move.l  DKRT_FXP_TAB+T_ROWS_N,%d0
1:  mvz.w   (%a4)+,%d1                  | the effect in this row (0: none)
    move.w  %d1,(%a1)+
    moveq   #8,%d2
2:  mvz.b   (%a5)+,%d3                  | the array index of this value
    moveq   #0,%d4
    tst.l   %d1
    beq.s   3f
    move.w  (0,%a3,%d3.l*2),%d4
3:  move.w  %d4,(%a0)+
    subq.l  #1,%d2
    bne.s   2b
    subq.l  #1,%d0
    bne.s   1b
    movem.l (%sp),%d2-%d4/%a4-%a5
    lea     20(%sp),%sp
    movea.l DKRT_FXP_TAB+T_R_SAVE,%a0
    jmp     (%a0)

    .globl fxp_init
fxp_init:
    tst.l   %a2                         | the function was given no array: nothing to do
    beq.s   9f
    addq.l  #1,DKRT_FXP_NINIT
    lea     -20(%sp),%sp
    movem.l %d2-%d4/%a3-%a4,(%sp)
    lea     DKRT_FXP_IDS,%a0
    lea     DKRT_FXP_DEFS,%a1
    lea     DKRT_FXP_TAB+T_VIDX,%a3
    move.l  DKRT_FXP_TAB+T_ROWS_N,%d0
1:  mvz.w   (%a0)+,%d1                  | the effect in this row (0: none)
    bne.s   2f
    lea     16(%a1),%a1                 | no effect: the row stays 0
    lea     8(%a3),%a3
    bra.s   4f
2:  move.l  DKRT_FXP_TAB+T_MARK_OFF,%d3
    move.l  DKRT_FXP_TAB+T_MARKVAL,%d4
    move.w  %d4,(0,%a2,%d3.l)
    moveq   #8,%d4
3:  mvz.w   (%a1)+,%d2                  | the default
    mvz.b   (%a3)+,%d3                  | the array index of this value
    move.w  %d2,(0,%a2,%d3.l*2)
    subq.l  #1,%d4
    bne.s   3b
4:  subq.l  #1,%d0
    bne.s   1b
    movem.l (%sp),%d2-%d4/%a3-%a4
    lea     20(%sp),%sp
9:  movem.l (%sp),%d2/%a2-%a5          | the displaced instructions
    lea     0x20(%sp),%sp
    move.l  DKRT_FXP_TAB+T_R_INIT,-(%sp)
    rts
"#
);
