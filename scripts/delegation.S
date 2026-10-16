/* U -> S delegated ECALL, nested S -> M ECALL, then MRET/SRET to U.
 * Differential snapshots verify trap CSR mutations even without CSR writes
 * in the retired instruction stream. */
    .section .text.init
    .globl _start
    .align 6
_start:
    csrw pmpcfg0, zero
    csrw pmpaddr0, zero
    .globl difftest_start
difftest_start:
    la t0, machine_trap
    csrw mtvec, t0
    la t0, supervisor_trap
    csrw stvec, t0
    li t0, -1
    csrw pmpaddr0, t0
    li t0, 0x1f
    csrw pmpcfg0, t0
    li t0, 0x100
    csrw medeleg, t0
    la t0, supervisor
    csrw mepc, t0
    li t0, 0x800
    csrw mstatus, t0
    mret
supervisor:
    la t0, user
    csrw sepc, t0
    li a0, 0
    sret
user:
    ecall
    li t0, 31
    bne a0, t0, fail_spin
    j pass_spin
supervisor_trap:
    csrr t0, scause
    li t1, 8
    bne t0, t1, fail_spin
    csrr t0, sepc
    la t1, user
    bne t0, t1, fail_spin
    ecall
    li t0, 7
    bne a0, t0, fail_spin
    li a0, 31
    csrr t0, sepc
    addi t0, t0, 4
    csrw sepc, t0
    sret
machine_trap:
    csrr t0, mcause
    li t1, 9
    bne t0, t1, fail_spin
    li a0, 7
    csrr t0, mepc
    addi t0, t0, 4
    csrw mepc, t0
    mret
    .globl pass_spin
pass_spin:
    j pass_spin
fail_spin:
    j fail_spin
