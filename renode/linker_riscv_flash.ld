OUTPUT_ARCH(riscv)
ENTRY(_reset)

MEMORY {
  FLASH (rx)  : ORIGIN = 0x42000000, LENGTH = 4M
  RAM   (rwx) : ORIGIN = 0x3FC80000, LENGTH = 400K
}

SECTIONS {
  .text.init : {
    KEEP(*(.text.init))
  } > FLASH

  .text : {
    *(.text*)
  } > FLASH

  .rodata : {
    *(.rodata*)
    *(.srodata*)
  } > FLASH

  .data : {
    _data_start = .;
    *(.data*)
    *(.sdata*)
    . = ALIGN(4);
    _data_end = .;
  } > RAM AT > FLASH
  _data_load_start = LOADADDR(.data);

  .bss (NOLOAD) : {
    _bss_start = .;
    *(.bss*)
    *(.sbss*)
    *(COMMON)
    . = ALIGN(4);
    _bss_end = .;
  } > RAM

  . = ALIGN(16);
  _stack_top = ORIGIN(RAM) + LENGTH(RAM);
}
