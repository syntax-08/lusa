OUTPUT_ARCH(arm)
ENTRY(_reset)

MEMORY {
  FLASH (rx)  : ORIGIN = 0x08000000, LENGTH = 1M
  RAM   (rwx) : ORIGIN = 0x20000000, LENGTH = 128K
}

SECTIONS {
  .vectors : {
    KEEP(*(.vectors))
  } > FLASH

  .text : {
    KEEP(*(.text.init))
    *(.text*)
    *(.glue_7)
    *(.glue_7t)
  } > FLASH

  .rodata : {
    *(.rodata*)
    *(.srodata*)
  } > FLASH

  .ARM.exidx : {
    *(.ARM.exidx*)
    *(.ARM.extab*)
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

  . = ALIGN(8);
  _stack_top = ORIGIN(RAM) + LENGTH(RAM);
}

@LUSA_INTERRUPT_PROVIDES@
