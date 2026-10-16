/* Native address-trigger regression for scripts/difftest.sh.
 * Tests execute/load/store breakpoints, two simultaneously active banks,
 * suppression in the M handler, MRET rearming, and the disabled MIE gate.
 * No optional tcontrol access or implementation-dependent full tdata1 reads.
 */
    .option norvc
    .option norelax
    .section .text.init
    .globl _start
_start:
    la t0, trap_handler
    csrw mtvec, t0
    li t4, 0
    csrwi tselect, 0
    la a6, execute_break
    mv a7, a6
    csrw tdata2, a6
    li t0, 0x20000044          /* type 2, M, execute */
    csrw tdata1, t0
    csrsi mstatus, 8
    li gp, 1
    li a0, 0
execute_break:
    addi a0, zero, 99
    j fail_spin
    bnez a0, fail_spin
    li t0, 1
    bne t4, t0, fail_spin

    li gp, 2
    la a7, watched_word
    csrw tdata2, a7
    li t0, 0x20000041          /* type 2, M, load */
    csrw tdata1, t0
    li a0, 77
    la a6, load_break
load_break:
    lw a0, 0(a7)
    j fail_spin
    li t0, 77
    bne a0, t0, fail_spin      /* faulting load never wrote its destination */

    li gp, 3
    csrwi tselect, 1
    csrw tdata2, a7
    li t0, 0x20000042          /* type 2, M, store */
    csrw tdata1, t0
    la a6, store_break
store_break:
    sw zero, 0(a7)
    j fail_spin
    li t0, 3
    bne t4, t0, fail_spin

    /* Both banks remain active. Clearing MIE suppresses the load breakpoint. */
    li gp, 4
    csrci mstatus, 8
    lw a0, 0(a7)
    li t0, 42
    bne a0, t0, fail_spin      /* the store breakpoint prevented modification */
    csrsi mstatus, 8
    la a6, first_bank_again
first_bank_again:
    lw a0, 0(a7)
    j fail_spin
    li t0, 4
    bne t4, t0, fail_spin
    j pass_spin

    .align 2
trap_handler:
    csrr t0, mcause
    li t1, 3
    bne t0, t1, fail_spin
    csrr t0, mtval
    bne t0, a7, fail_spin
    csrr t0, mepc
    bne t0, a6, fail_spin
    /* In load stages this reads the watched word without recursively firing. */
    lw t2, 0(a7)
    addi t4, t4, 1
    addi t0, t0, 8
    csrw mepc, t0
    mret

    .align 4
    .globl pass_spin
pass_spin:
    j pass_spin
    .align 4
    .globl fail_spin
fail_spin:
    j fail_spin

    .data
    .align 2
watched_word:
    .word 42
