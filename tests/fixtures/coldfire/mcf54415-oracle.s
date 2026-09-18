/*
 * Original MCF5441x decode fixture derived from the public NXP manuals.
 * It contains no firmware bytes and no text copied from GNU test sources.
 */

        .text
        .globl  oracle_start
oracle_start:
        movclr.l %acc0,%d1
        mac.l   %d0,%d1,%acc0
        mac.w   %d0u,%d1u,(%a0),%d2,%acc1
        msac.w  %d2u,%d3u,(%a1)+,%d4,%acc3
        move.l  %acc1,%acc2
        move.l  %d3,%accext01
        move.l  %accext23,%d4
        movec   %d0,%acr4
        movec   %d1,%acr5
        movec   %d2,%acr6
        movec   %d3,%acr7
        movec   %d4,%rgpiobar
        mov3q   #-1,%d2
        bitrev.l %d3
        byterev.l %d4
        ff1.l   %d5
        nop
