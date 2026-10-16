/* Directed PMP regression shared by the model and Spike.
 * Checks MPRV, unlocked M bypass, locked M/S permissions, immutable PMP CSRs,
 * precise access-fault metadata, and MRET back into S-mode.
 * Build with scripts/difftest.ld; pass the ELF to scripts/difftest.sh.
 */
    .option norvc
    .option norelax
    .section .text.init
    .globl _start
_start:
    /* Establish the same PMP baseline on both simulator platforms. */
    csrw    pmpcfg0, zero
    csrw    pmpaddr0, zero
    .globl difftest_start
difftest_start:
    la t0, trap_handler
    csrw mtvec, t0
    li t4, 0                   /* number of expected traps */
    li t6, 7                   /* store access fault */
    la a7, protected_word

    /* A fallback region permits S-mode code. Entry zero is read-only data. */
    li t0, -1
    csrw pmpaddr15, t0
    li t0, 0x1f000000
    csrw pmpcfg3, t0
    srli t0, a7, 2
    csrw pmpaddr0, t0
    li t0, 0x11                /* NA4 | R, unlocked */
    csrw pmpcfg0, t0

    li gp, 1
    li t0, 0x20000             /* MPRV=1, MPP=U */
    csrw mstatus, t0
    lw a0, 0(a7)
    li t0, 42
    bne a0, t0, fail_spin
    la a6, denied_mprv_store
denied_mprv_store:
    sw zero, 0(a7)
    j fail_spin
    li t0, 1
    bne t4, t0, fail_spin

    /* Ordinary M accesses bypass permissions on an unlocked entry. */
    li gp, 2
    csrw mstatus, zero
    li a0, 43
    sw a0, 0(a7)
    lw a1, 0(a7)
    bne a0, a1, fail_spin

    /* Locking makes the rule immutable and enforces it for M too. */
    li gp, 3
    li t0, 0x91                /* L | NA4 | R */
    csrw pmpcfg0, t0
    csrw pmpaddr0, zero
    csrw pmpcfg0, zero
    csrr t0, pmpaddr0
    srli t1, a7, 2
    bne t0, t1, fail_spin
    csrr t0, pmpcfg0
    li t1, 0x91
    bne t0, t1, fail_spin
    la a6, denied_machine_store
denied_machine_store:
    sw zero, 0(a7)
    j fail_spin
    li t0, 2
    bne t4, t0, fail_spin
    lw a0, 0(a7)
    li t0, 43
    bne a0, t0, fail_spin

    /* Lower privilege uses the same physical rule and can return from a fault. */
    li gp, 4
    la t0, supervisor_body
    csrw mepc, t0
    li t0, 0x800               /* MPP=S */
    csrw mstatus, t0
    mret
supervisor_body:
    lw a0, 0(a7)
    li t0, 43
    bne a0, t0, fail_spin
    la a6, denied_supervisor_store
denied_supervisor_store:
    sw zero, 0(a7)
    j fail_spin
    li t0, 3
    bne t4, t0, fail_spin
    lw a0, 0(a7)
    li t0, 43
    bne a0, t0, fail_spin
    j pass_spin

    .align 2
trap_handler:
    csrr t0, mcause
    bne t0, t6, fail_spin
    csrr t0, mtval
    bne t0, a7, fail_spin
    csrr t0, mepc
    bne t0, a6, fail_spin
    addi t4, t4, 1
    addi t0, t0, 8             /* skip faulting store and failure sentinel */
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
protected_word:
    .word 42
