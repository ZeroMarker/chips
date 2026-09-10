/* Minimal bare-metal RV32IM program for the model smoke test.
 *
 * `scripts/riscv-smoke.sh` assembles this with the RISC-V cross toolchain,
 * extracts the raw image, runs it on the model, and checks the final register
 * state. The expected values are asserted there, one per source line below.
 *
 * The program deliberately sticks to the base integer and M extensions plus
 * registers, so it exercises the assembler-to-model path without needing a
 * runtime, a linker script, or a trap handler.
 */

    .section .text
    .globl _start

_start:
    li   a0, 6              /* a0 = 6                    -> x10 = 0x00000006 */
    li   a1, 7              /* a1 = 7                    -> x11 = 0x00000007 */
    mul  a2, a0, a1         /* a2 = 42                   -> x12 = 0x0000002a */
    li   a3, -20            /* a3 = -20                  -> x13 = 0xffffffec */
    li   a4, 3              /* a4 = 3                    -> x14 = 0x00000003 */
    div  a5, a3, a4         /* a5 = -20 / 3 = -6         -> x15 = 0xfffffffa */
    rem  a6, a3, a4         /* a6 = -20 % 3 = -2         -> x16 = 0xfffffffe */
    divu a7, zero, zero     /* a7 = 0 / 0 -> all ones    -> x17 = 0xffffffff */
    ebreak                  /* halts the run loop; exit code stays 0 */
