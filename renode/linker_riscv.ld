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
    _data_start = .;
    *(.data*)
    *(.sdata*)
    . = ALIGN(4);
    _data_end = .;
  } > RAM
  _data_load_start = LOADADDR(.data);

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

@LUSA_INTERRUPT_PROVIDES@
