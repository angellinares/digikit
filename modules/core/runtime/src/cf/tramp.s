/* core/runtime: hook trampoline and cache maintenance (docs/design/digikit-runtime.md).
 *
 * Board facts (which UART is the console, where DDR is) come from the device files through build/gen/device.inc; the register
 * offsets of the MCF5441x itself are named here.
 *
 * dkrt_loop_hook replaces the target of the `jsr` at the top of the UI task's event loop (the call that pops the next event
 * from the UI queue; the queue pointer is on the stack, the stock function returns the event in d0). It runs the runtime's
 * tick, then continues in the stock pop as if it had been called directly: the stack is as the caller left it.
 * d0/d1/a0/a1 are the registers the C ABI lets dkrt_tick clobber; the loop has nothing live in them at this call (it
 * has just pushed the argument, and the result comes back in d0). d2-d7/a2-a6 are preserved by the C function.
 * fw_queue_pop is the stock function (anchor cf.rtos.fn.queue_pop).
 */
    .include "device.inc"
    .include "dk_fw_view.inc"
    .include "dk_fw_rtos.inc"
    .equ MCF5441X_UART_USR_OFF, 4       /* UART status register, offset from the UART base; bit 3 is TXEMP (transmitter empty) */
    .equ MCF5441X_RCM_RCR, 0xEC090000   /* reset controller: reset control register; 0x80 = software reset */

    .text
    .even
    .globl dkrt_loop_hook
dkrt_loop_hook:
    dk_fw_view_ui_loop_hook dkrt_tick, fw_queue_pop

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

/* dkrt_boot_hook: the hook at the entry of the main-OS task (a `hook` op: the displaced prologue, `link a6,#-0xa8; movem.l ...`,
 * runs in the tail `dkrt_boot_next`, then the task goes on). It runs dkrt_boot (the selection record and the boot-time loads) with
 * the stack as the task entry left it. d0/d1/a0/a1 are saved; d2-d7/a2-a6 are the C function's to keep. */
    .globl dkrt_boot_hook
dkrt_boot_hook:
    dk_fw_rtos_main_os_task_hook dkrt_boot, dkrt_boot_next

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

/* dkrt_midi_hook: the hook in the MIDI task's dispatch of a gathered USB SysEx message (docs/design/digikit-upload.md). At the hook the
 * stack holds the router's arguments, pushed by the task: 0(sp) = message, 4(sp) = length, 8(sp) = source; the displaced instructions
 * (`movea.l (0,A2,D1*4),A0; jsr (A0)`) call the router. d0/d1/a0/a1 are saved (D1 is live: it indexes the router table). dkrt_midi_rx
 * returns 1 for our messages: continue at dkrt_midi_ret (after the call, where the task pops its arguments and frees the blocks), else
 * run the displaced instructions in dkrt_midi_next. movem and lea leave the condition codes alone. */
    .globl dkrt_midi_hook
dkrt_midi_hook:
    lea     -16(%sp),%sp
    movem.l %d0-%d1/%a0-%a1,(%sp)
    move.l  24(%sp),-(%sp)      /* source */
    move.l  24(%sp),-(%sp)      /* length */
    move.l  24(%sp),-(%sp)      /* message */
    jsr     dkrt_midi_rx
    lea     12(%sp),%sp
    tst.l   %d0
    movem.l (%sp),%d0-%d1/%a0-%a1
    lea     16(%sp),%sp
    bne.s   1f
    jmp     dkrt_midi_next
1:  jmp     dkrt_midi_ret

/* dkrt_exc_hook: the function the firmware's default exception handler calls instead of its own (finding 46). The handler has set sr to 0x2700,
 * saved d0, d1, a0, a1 below the exception frame, and pushed the frame pointer: 0(sp) = return address into the handler, 4(sp) = frame pointer. d2-d7
 * and a2-a6 are still the faulting code's, so they are saved here before any compiled code runs; dkrt_exc(frame, saved) records and resets and
 * does not return. (ColdFire movem has no predecrement form: lea, then movem.) */
    .globl dkrt_exc_hook
dkrt_exc_hook:
    lea     -44(%sp),%sp
    movem.l %d2-%d7/%a2-%a6,(%sp)
    move.l  %sp,-(%sp)          /* saved */
    move.l  52(%sp),-(%sp)      /* frame: 4 (saved) + 44 + 4 (return address) = 52, read before the push */
    jsr     dkrt_exc
1:  bra.s   1b

/* dkrt_pagekey_hook, dkrt_pageturn_hook: the hooks at the entry of the machine page's key handler and turn handler
 * (cf.page.hook.source_key, cf.page.hook.source_turn; SourcePageView on the Digitakt II, MultiSourcePageView on the Digitone II). At
 * the entry 0(sp) = the return address, 4(sp) = the view, 8(sp) = the event; d0/d1/a0/a1 are free. The C function gets (view, event)
 * and returns; the stock handler then runs with the stack as it was, so the hook never consumes an event (src/rs/src/pageclick.rs). */
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
    jmp     dkrt_pageturn_next
