/* Linker script for scripts/difftest.S.
 *
 * The same page structure riscv-tests' env/p/link.ld uses, because the program
 * reports through the same tohost convention and the harness reads that symbol
 * out of the image: .text.init at the reset vector, then tohost on the next page
 * boundary, then .text/.data/.bss. Keeping the layout identical means the
 * harness needs no special case for one program versus a suite.
 */

OUTPUT_ARCH( "riscv" )
ENTRY(_start)

SECTIONS
{
  . = 0x80000000;
  .text.init : { *(.text.init) }
  . = ALIGN(0x1000);
  .tohost : { *(.tohost) }
  . = ALIGN(0x1000);
  .text : { *(.text) }
  . = ALIGN(0x1000);
  .data : { *(.data) }
  .bss : { *(.bss) }
  _end = .;
}
