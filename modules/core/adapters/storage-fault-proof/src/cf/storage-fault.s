/* Private DT2 1.17 observer. Each replace hook performs exactly the displaced
 * MOVE.L IRQSTAT,D0 once, then jumps to site+6, never the displaced-code tail.
 * MOVEM/LEA preserve the original load's CCR before SR is saved. The single
 * memory OR merges a nested observer's bits without a load/store lost update.
 */
    .text
    .even
    .macro fault_hook name, site
    .globl \name
\name:
    move.l  0xfc0cc030,%d0
    lea     -8(%sp),%sp
    movem.l %d1,(%sp)
    move.w  %sr,%d1
    move.w  %d1,4(%sp)
    move.l  %d0,%d1
    and.l   #0x00310000,%d1
    or.l    %d1,storage_fault_bits
    move.w  4(%sp),%d1
    move.w  %d1,%sr
    movem.l (%sp),%d1
    lea     8(%sp),%sp
    jmp     \site+6
    .endm

    fault_hook storage_fault_hook_a,storage_fault_site_a
    fault_hook storage_fault_hook_b,storage_fault_site_b
    fault_hook storage_fault_hook_c,storage_fault_site_c

    .bss
    .balign 4
    .globl storage_fault_bits
storage_fault_bits:
    .space 4
