/* core/runtime: cache maintenance, interrupt mask, soft reset and the machine page hooks (docs/design/digikit-runtime.md).
 *
 * The hooks of the UI loop, the main-OS task entry and the MIDI receive, and the exception hook, are in core/platform (R3b).
 *
 * Board facts (which UART is the console, where DDR is) come from the device files through build/gen/device.inc; the register
 * offsets of the MCF5441x itself are named here.
 */
    .include "device.inc"
    .equ MCF5441X_UART_USR_OFF, 4       /* UART status register, offset from the UART base; bit 3 is TXEMP (transmitter empty) */
    .equ MCF5441X_RCM_RCR, 0xEC090000   /* reset controller: reset control register; 0x80 = software reset */

    .text
    .even
/* void dkrt_flush(u32 start, u32 len): CPUSHL BC for every 16-byte line of [start, start+len): write back dirty data
 * lines and invalidate the line in the data and instruction caches. Needed after code or data that will be fetched as
 * instructions has been written through the data cache (finding 07: the cache is not coherent on hardware). */
    .globl dkrt_flush
dkrt_flush:
    move.l  4(%sp),%a0
    move.l  8(%sp),%d1
    move.l  %a0,%d0
    and.l   #15,%d0
    add.l   %d0,%d1             /* include the bytes before the first line start */
    move.l  %a0,%d0
    and.l   #-16,%d0
    move.l  %d0,%a0
    add.l   #15,%d1
    lsr.l   #4,%d1              /* number of lines */
    beq.s   2f
1:  cpushl  %bc,(%a0)
    lea     16(%a0),%a0
    subq.l  #1,%d1
    bne.s   1b
2:  rts

/* u32 dkrt_irq_mask(void): status register to 0x2700, returns the old one. void dkrt_irq_restore(u32 sr). */
    .globl dkrt_irq_mask
dkrt_irq_mask:
    clr.l   %d0
    move.w  %sr,%d0
    move.w  #0x2700,%sr
    rts
    .globl dkrt_irq_restore
dkrt_irq_restore:
    move.l  4(%sp),%d0
    move.w  %d0,%sr
    rts

/* void dkrt_soft_reset(void): the firmware's own sequence (finding 36, section 3), interrupts already masked: wait (bounded) for
 * the console UART transmitter to empty (USR bit 3), clear the first word of DDR through the uncached alias (a stale magic
 * there would start the next boot in console mode), 25 nops, then 0x80 into RCM.RCR. Does not return. */
    .globl dkrt_soft_reset
dkrt_soft_reset:
    move.w  #0x2700,%sr
    move.l  #200000,%d1
1:  move.b  CONSOLE_UART_BASE + MCF5441X_UART_USR_OFF,%d0
    btst    #3,%d0
    bne.s   2f
    subq.l  #1,%d1
    bne.s   1b
2:  clr.l   DDR_UNCACHED_ALIAS
    .rept 25
    nop
    .endr
    moveq   #-128,%d0
    move.b  %d0,MCF5441X_RCM_RCR
3:  bra.s   3b

/* dkrt_pagekey_hook, dkrt_pageturn_hook: the hooks at the entry of the machine page's key handler and turn handler
 * (cf.page.hook.source_key, cf.page.hook.source_turn; SourcePageView on the Digitakt II, MultiSourcePageView on the Digitone II). At
 * the entry 0(sp) = the return address, 4(sp) = the view, 8(sp) = the event; d0/d1/a0/a1 are free. The C function gets (view, event)
 * and returns; keys and ordinary PARAM turns continue with the original stack. A native page's LOCAL/unused turn returns one,
 * which the stock dispatcher treats as consumed (../services/machines/src/slots.rs). */
    .globl dkrt_pagekey_hook
dkrt_pagekey_hook:
    move.l  8(%sp),-(%sp)       /* event */
    move.l  8(%sp),-(%sp)       /* view: 4(sp) at the entry, 8 after one push */
    jsr     dkrt_page_key
    addq.l  #8,%sp
    jmp     dkrt_pagekey_next

    .globl dkrt_pageturn_hook
dkrt_pageturn_hook:
    move.l  8(%sp),-(%sp)
    move.l  8(%sp),-(%sp)
    jsr     dkrt_page_turn
    addq.l  #8,%sp
    tst.b   %d0
    beq.s   1f
    moveq   #1,%d0                 /* native dispatcher consumes a nonzero D0b */
    rts
1:  jmp     dkrt_pageturn_next
