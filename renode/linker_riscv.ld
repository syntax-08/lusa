OUTPUT_ARCH(riscv)
ENTRY(_reset)

MEMORY {
  RAM (rwx) : ORIGIN = 0x80000000, LENGTH = 128M
}

SECTIONS {
  .text.init : {
    KEEP(*(.text.init))
  } > RAM

  .text : {
    *(.text*)
  } > RAM

  .rodata : {
    *(.rodata*)
    *(.srodata*)
  } > RAM

  .data : {
    *(.data*)
    *(.sdata*)
  } > RAM

  .bss : {
    _bss_start = .;
    *(.bss*)
    *(.sbss*)
    *(COMMON)
    _bss_end = .;
  } > RAM

  . = ALIGN(16);
  . = . + 0x10000;
  _stack_top = .;
}
