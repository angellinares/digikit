/* Native callback page draw and PARAM-code seams; source function entries.
 * Stack is return, view, canvas/encoder. C preserves all nonvolatile registers.
 * -1 from code means pass through; 0 is an intentional unused/LOCAL mapping. */
    .text
    .globl mf_native_draw_hook
mf_native_draw_hook:
    move.l  8(%sp),-(%sp)
    move.l  8(%sp),-(%sp)
    jsr     mf_native_draw
    addq.l  #8,%sp
    tst.l   %d0
    beq.s   1f
    cmpi.l  #2,%d0
    beq.s   2f
    rts
1:  jmp     mf_native_draw_next
2:  /* Original stack is return, view, canvas. Push canvas then view;
       each callee sees return, view, canvas. Both calls unwind fully. */
    move.l  8(%sp),-(%sp)
    move.l  8(%sp),-(%sp)
    jsr     mf_native_draw_next
    addq.l  #8,%sp
    move.l  8(%sp),-(%sp)
    move.l  8(%sp),-(%sp)
    jsr     mf_native_overlay
    addq.l  #8,%sp
    rts

    .globl mf_native_code_hook
mf_native_code_hook:
    move.l  8(%sp),-(%sp)
    move.l  8(%sp),-(%sp)
    jsr     mf_native_code
    addq.l  #8,%sp
    cmpi.l  #-1,%d0
    beq.s   1f
    rts
1:  jmp     mf_native_code_next
