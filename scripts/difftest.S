/* Differential-test program: a broad instruction mix with no optional-CSR
 * feature detection, for comparing the model against Spike.
 *
 * scripts/difftest.sh runs this and compares the two instruction traces step by
 * step. It exists because the riscv-tests binaries cannot serve that purpose:
 * their setup prologue probes mnstatus, pmpaddr0 and medeleg, and since the model
 * and Spike implement different subsets of those, the two walk through the same
 * source by different routes and their streams part company before any real
 * arithmetic happens. That is a true statement about the programs, not about the
 * cores, and it means the suites are the wrong input for a trace comparison.
 *
 * This program probes nothing optional, so the two traces can be compared
 * directly.
 *
 * It reports its verdict by settling into one of two distinct spin addresses,
 * `pass_spin` or `fail_spin`, which the harness reads out of the symbol table.
 * It does *not* write the HTIF `tohost` register, even though that is the obvious
 * way to end a bare-metal program: Spike's built-in HTIF speaks a different
 * protocol from the one riscv-tests uses, and the proxy kernel that translated
 * between them is no longer part of riscv-isa-sim, so no single tohost value
 * terminates correctly on both sides.
 */

/* The test number lives in gp, which riscv-tests also uses: it is callee-saved
 * and untouched by everything below, and naming it keeps the failure paths short. */
#define TESTNUM gp

    .section .text.init
    .globl _start
    .align 6
_start:
    /* Install the trap handler first, so an unexpected trap is reported rather
     * than looping. The handler is a distinct check number from any real failure,
     * which is what lets a mismatch say "a trap happened" instead of "a value was
     * wrong". */
    la      t0, trap_vector
    csrw    mtvec, t0
    li      TESTNUM, 0

    li      TESTNUM, 10
    /* --- integer register file: each ALU op, and its edge cases --- */
    li      a0, 0x7fffffff
    li      a1, 1
    add     a2, a0, a1              /* wraps to 0x80000000 */
    li      t0, 0x80000000
    bne     a2, t0, fail

    sub     a2, a0, a1
    li      t0, 0x7ffffffe
    bne     a2, t0, fail

    /* Shifts, including shamt taken from a register and masked to 5 bits. */
    li      a0, 0x80000001
    li      a1, 4
    sll     a2, a0, a1
    li      t0, 0x00000010
    bne     a2, t0, fail

    li      a1, 36                  /* shamt 36 & 31 = 4 */
    sll     a2, a0, a1
    li      t0, 0x00000010
    bne     a2, t0, fail

    sra     a2, a0, a1              /* arithmetic: sign preserved */
    li      t0, 0xf8000000
    bne     a2, t0, fail

    srl     a2, a0, a1              /* logical: sign cleared */
    li      t0, 0x08000000
    bne     a2, t0, fail
    srli    a2, a0, 4               /* the immediate form of the same thing */
    li      t0, 0x08000000
    bne     a2, t0, fail

    /* Sign and zero extension. */
    li      a0, -1
    srai    a2, a0, 4
    li      t0, 0xffffffff
    bne     a2, t0, fail
    srli    a2, a0, 28
    li      t0, 0xf
    bne     a2, t0, fail

    /* Signed vs unsigned comparison, which is where the two differ: -1 is less
     * than 1 when read as a signed value and greater when read as unsigned. */
    li      a0, -1
    li      a1, 1
    slt     a2, a0, a1              /* -1 < 1 signed: true */
    beqz    a2, fail
    sltu    a2, a0, a1              /* 0xffffffff < 1 unsigned: false */
    bnez    a2, fail
    slt     a2, a1, a0              /* 1 < -1 signed: false */
    bnez    a2, fail
    sltu    a2, a1, a0              /* 1 < 0xffffffff unsigned: true */
    beqz    a2, fail

    li      TESTNUM, 20
    /* --- M extension, including the architecturally specified edges --- */
    li      a0, 6
    li      a1, 7
    mul     a2, a0, a1
    li      t0, 42
    bne     a2, t0, fail

    /* High halves: -1 * 1 is -1 signed but 0xffffffff unsigned, so the two
     * differ and both are worth checking. */
    li      a0, -1
    li      a1, 1
    mulh    a2, a0, a1              /* (-1 * 1) >> 32 = -1 */
    li      t0, -1
    bne     a2, t0, fail
    mulhu   a2, a0, a1              /* (0xffffffff * 1) >> 32 = 0 */
    bnez    a2, fail
    mulhsu  a2, a0, a1              /* (-1 * 1) signed by unsigned: -1 */
    li      t0, -1
    bne     a2, t0, fail

    /* Division truncates toward zero and the remainder takes the dividend's
     * sign, which is where the two disagree for a negative dividend. */
    li      a0, -20
    li      a1, 3
    div     a2, a0, a1              /* -20 / 3 = -6, not -7 */
    li      t0, -6
    bne     a2, t0, fail
    rem     a2, a0, a1              /* -20 % 3 = -2, not 2 */
    li      t0, -2
    bne     a2, t0, fail

    /* Division by zero: the quotient is all ones and the remainder is the
     * dividend, for both signednesses. */
    li      a0, 100
    li      a1, 0
    divu    a2, a0, a1
    li      t0, -1
    bne     a2, t0, fail
    remu    a2, a0, a1
    li      t0, 100
    bne     a2, t0, fail
    div     a2, a0, a1
    li      t0, -1
    bne     a2, t0, fail
    rem     a2, a0, a1
    li      t0, 100
    bne     a2, t0, fail

    /* The signed overflow case: the most negative value divided by -1 yields the
     * most negative value, and the remainder is 0. No exception is raised. */
    lui     a0, 0x80000             /* 0x80000000 */
    li      a1, -1
    div     a2, a0, a1
    lui     t0, 0x80000
    bne     a2, t0, fail
    rem     a2, a0, a1
    bnez    a2, fail

    li      TESTNUM, 30
    /* --- memory: aligned, unaligned, and each width --- */
    la      s0, data
    /* Build 0xdeadbeef without a wide immediate: lui supplies the top 20 bits
     * and the shifts and ors assemble the rest. */
    lui     a0, 0xdeadc
    addi    a0, a0, -273            /* 0xdeadc000 - 0x111 = 0xdeadbeef */
    sw      a0, 0(s0)
    lw      a1, 0(s0)
    bne     a1, a0, fail            /* a word survives a round trip */

    /* Little-endian order, checked one byte at a time so no wide immediate is
     * needed: byte n of the word comes from address n. */
    li      a0, 0x7B
    sb      a0, 0(s0)
    lw      a1, 0(s0)
    andi    a1, a1, 0xff
    li      t0, 0x7B
    bne     a1, t0, fail

    li      a0, 0x5C
    sb      a0, 1(s0)
    lw      a1, 0(s0)
    andi    a1, a1, 0xff
    li      t0, 0x7B                /* offset 0 still holds 0x7B */
    bne     a1, t0, fail
    lw      a1, 0(s0)               /* reload: the mask above discarded the rest */
    srli    a1, a1, 8
    andi    a1, a1, 0xff
    li      t0, 0x5C                /* and offset 1 holds what was just stored */
    bne     a1, t0, fail

    /* No misaligned access appears in this program, and that is deliberate.
     *
     * The base ISA leaves a misaligned *data* access implementation-defined: it
     * may complete or raise an address-misaligned exception, and RV32 without
     * Zicclsm has no rule that picks one. Spike raises the exception; this model
     * completes the access, because riscv-tests `ma_data` and real hardware both
     * require the value to come back, and a golden reference has to agree with
     * the thing it is a reference for.
     *
     * Both choices are correct, so a differential comparison cannot ask the two
     * to agree about one — and a single divergent instruction shifts every
     * instruction after it, so leaving it in would make the whole comparison
     * meaningless. The model's behaviour is covered by the conformance suite
     * instead (rv32ui-p-ma_data and the rv32mi-p-*-misaligned tests), where the
     * expected result is stated rather than negotiated. */

    /* Sign and zero extension, at each width. Every check stores the bytes it
     * depends on rather than assuming what an earlier check left behind — the
     * halfword at data+0 depends on data+1 as much as on data+0, and getting
     * that wrong makes the test assert something the architecture never said. */
    li      a0, 0x00
    sb      a0, 1(s0)                /* the halfword is 0x0080, so it is positive */
    li      a0, 0x80
    sb      a0, 0(s0)
    lbu     a1, 0(s0)
    li      t0, 0x80
    bne     a1, t0, fail            /* lbu zero-extends */
    lb      a1, 0(s0)
    bltz    a1, ok_lb               /* 0x80 as a signed byte is negative */
    j       fail
ok_lb:
    lhu     a1, 0(s0)
    li      t0, 0x80
    bne     a1, t0, fail
    lh      a1, 0(s0)
    bgez    a1, ok_lh               /* the halfword 0x0080 is positive */
    j       fail
ok_lh:
    /* The same 0xff byte: lb sign-extends it to -1, while lh reads the halfword
     * 0x00ff, which is still positive. That difference is the whole point of the
     * two widths, and it is visible only if the neighbouring byte is zero. */
    li      a0, 0xFF
    sb      a0, 0(s0)
    lb      a1, 0(s0)
    li      t0, -1
    bne     a1, t0, fail
    lh      a1, 0(s0)
    bgez    a1, ok_lh2              /* the halfword 0x00ff is still positive */
    j       fail
ok_lh2:

    li      TESTNUM, 40
    /* --- branches, both directions, both signednesses --- */
    li      a0, 5
    li      a1, 5
    beq     a0, a1, 1f
    j       fail
1:  bne     a0, a1, fail
    blt     a1, a0, fail            /* equal values are not less */
    bge     a0, a1, 1f
    j       fail
1:  bltu    a0, a1, fail            /* ...unsigned either */
    bgeu    a1, a0, 1f
    j       fail
1:  blt     a0, a1, fail
    bltu    a0, a1, fail
    bgeu    a0, a1, 1f
    j       fail
1:
    /* A negative and a positive operand is the only case where the signed and
     * unsigned orders disagree, and both directions have to be checked: -1 < 1
     * signed but 0xffffffff > 1 unsigned, and the reverse pair says the same
     * thing. */
    li      a0, -1
    li      a1, 1
    blt     a0, a1, 1f
    j       fail
1:  bltu    a0, a1, fail
    blt     a1, a0, fail
    bltu    a1, a0, 1f
    j       fail
1:

    li      TESTNUM, 50
    /* --- jumps and links --- */
    /* `jal` links the address of the instruction *after* it, so comparing ra
     * against that address checks the jump and the link in one go, and stays
     * correct no matter how the assembler lays the program out. */
    la      a0, 1f
    jal     ra, subroutine
1:  sub     a1, ra, a0
    bnez    a1, fail               /* ra is the address just after the jal */

    /* AUIPC supplies the upper bits of its own address, so two consecutive
     * auipcs must differ by exactly one instruction. */
    auipc   a0, 0
    auipc   a1, 0
    sub     a2, a1, a0
    li      t0, 4
    bne     a2, t0, fail

    /* And `jalr` with rd = x0 is the return. */
    la      a0, 1f
    jal     ra, subroutine
1:  sub     a1, ra, a0
    bnez    a1, fail               /* ra is the address just after the jal */

    li      TESTNUM, 60
    /* --- CSRs --- */
    /* Only `mscratch` is read here, and deliberately.
     *
     * The comparison is over register *values*, so any register whose value the
     * architecture leaves implementation-defined would report a difference that
     * is not one: `misa` names the extensions an implementation provides, so a
     * core with compressed instructions or non-standard extensions legitimately
     * reads a larger mask than one without them; and `cycle`/`time`/`instret` are
     * counters whose values have no meaning outside their own machine.
     *
     * Those are checked elsewhere, where the expectation is stated rather than
     * negotiated: the model's own tests pin `misa` exactly, and the conformance
     * suite checks the counters. What is left for a *comparison* is the storage a
     * CSR provides, which both implementations must agree on byte for byte. */
    li      a0, 0x1234
    csrw    mscratch, a0
    csrr    a1, mscratch
    bne     a1, a0, fail

    /* Which forms write, and which do not, is a detail worth pinning: CSRRW
     * writes unconditionally, so with rs1 = x0 it stores zero. CSRRS and CSRRC
     * write only when rs1 names a non-zero value, so with rs1 = x0 they are pure
     * reads and the value survives. Getting this backwards silently destroys the
     * register rather than failing, which is the sort of thing that hides in
     * firmware until something else reads the CSR. */
    csrrw   a2, mscratch, x0        /* always writes: mscratch becomes 0 */
    bne     a2, a0, fail            /* but rd still receives the old value */
    csrr    a2, mscratch
    bnez    a2, fail                /* and the CSR did not keep it */

    li      a0, 0x1234
    csrw    mscratch, a0
    csrrs   a2, mscratch, x0        /* does not write */
    bne     a2, a0, fail
    csrr    a2, mscratch
    bne     a2, a0, fail

    /* CSRRS and CSRRC are read-*modify*-write: they set and clear bits rather
     * than replacing the register, and rd receives the old value either way. Both
     * properties are checked here, because reading rd as the new value is an easy
     * and entirely invisible mistake to make. */
    li      a0, 0x1234
    csrw    mscratch, a0
    li      a3, 0x321
    slli    a3, a3, 5              /* 0x6420: the bits to set */

    csrrs   a4, mscratch, a3
    or      a5, a0, a3             /* 0x7634: what it must leave behind */
    csrr    a1, mscratch
    bne     a1, a5, fail
    bne     a4, a0, fail            /* rd got the old value, not the new one */

    csrrc   a4, mscratch, a0       /* clear exactly what the last one set */
    li      a6, -64
    and     a6, a3, a6             /* 0x6420 with its low bits gone = 0x6400 */
    csrr    a1, mscratch
    bne     a1, a6, fail
    bne     a4, a5, fail           /* and again the old value, not the new one */

    li      TESTNUM, 90
    li      a0, 0x7c0
    csrrs   a2, 0x7c0, x0          /* must trap */
    j       fail                   /* only reached if it did not */

    /* Every check passed. The verdict is the address this program settles at. */
    li      TESTNUM, 1
    j       pass_spin

    .align 2
trap_vector:
    /* Two cases, and telling them apart is the point. TESTNUM 90 means the trap
     * was the one the illegal-CSR check provoked, so resume past it. Anything else
     * is an unexpected trap, and 1001 is a check number of its own so a mismatch
     * distinguishes "a trap happened" from "a value was wrong" — different bugs
     * with different fixes. */
    li      t0, 90
    bne     TESTNUM, t0, unexpected_trap
    csrr    t0, mepc
    addi    t0, t0, 8               /* past the faulting access *and* the j fail */
    csrw    mepc, t0
    li      TESTNUM, 91
    mret
unexpected_trap:
    li      TESTNUM, 1001
    j       fail_spin

fail:
    /* TESTNUM still names the check that failed, so a debugger stopped here can
     * read it out of gp. The verdict itself is the address: fail_spin rather than
     * pass_spin. */
    j       fail_spin

    /* Placed after the last fall-through path rather than in the middle of the
     * program: `ret` is a bare `jalr`, so anything that falls through into it
     * jumps to whatever ra happens to hold and the program loops. */
    .align 2
subroutine:
    jalr    zero, ra, 0            /* ret */

    /* Two distinct spin loops, one for each verdict.
     *
     * The obvious way to end a bare-metal program is to write the result to the
     * HTIF `tohost` register, which is what riscv-tests does and what the model's
     * --htif mode reports from. But Spike's built-in HTIF speaks a *different*
     * protocol, and the proxy kernel that used to translate between them is no
     * longer part of riscv-isa-sim, so a program that terminates correctly for
     * one of them will not terminate for the other. Comparing instruction traces
     * does not need a verdict channel at all: these two addresses are, and a
     * harness that checks the final PC has the result. */
    .align 4
    .globl  pass_spin
pass_spin:
1:  j       1b

    .align 4
    .globl  fail_spin
fail_spin:
1:  j       1b
    .size   fromhost, 8

    .data
    .align 4
data:
    .space  64
