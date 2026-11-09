| core/overdrive on the Digitakt II: the value text of OVR TYPE (the stock OVR Routing row, through core/params'
| param_edits): "PRE " or "PST " (bit 0 of raw >> 8), then the flavour (raw >> 9). The stock formatters' convention: cdecl,
| `raw` at 4(sp) in 8.8, `buf` at 8(sp); d0/d1/a0/a1 scratch.
|
|     void od_type_fmt(int raw, char *buf)

    .text
    .globl od_type_fmt
od_type_fmt:
    move.l  4(%sp),%d0
    asr.l   #8,%d0
    movea.l 8(%sp),%a1
    lea     ot_pre,%a0
    btst    #0,%d0
    beq.s   1f
    lea     ot_pst,%a0
1:  move.b  (%a0)+,(%a1)+
    bne.s   1b
    subq.l  #1,%a1                  | back onto the NUL
    asr.l   #1,%d0
    andi.l  #7,%d0
    lsl.l   #3,%d0                  | 8 bytes per name
    lea     ot_names,%a0
    adda.l  %d0,%a0
2:  move.b  (%a0)+,(%a1)+
    bne.s   2b
    rts

    .section .rodata
ot_pre: .asciz "PRE "
ot_pst: .asciz "PST "
ot_names:
    .ascii "STK\0\0\0\0\0"
    .ascii "TNH4\0\0\0\0"
    .ascii "HARD\0\0\0\0"
    .ascii "SOFT\0\0\0\0"
    .ascii "TUBE\0\0\0\0"
    .ascii "FOLD\0\0\0\0"
    .ascii "FUZZ\0\0\0\0"
    .ascii "SQR\0\0\0\0\0"
