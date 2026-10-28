/* Lusa bare-metal startup for the Renode RISC-V `virt` machines.
 *
 * Renode enters at the ELF entry symbol (`_reset`).  The generated Lusa C
 * translation unit exports the user's `#[entry]` function and the build
 * renames it to `lusa_entry`; initialise the stack pointer and BSS first,
 * then call it.  This stub uses no libc.
 */
.section .text.init, "ax"
.globl _reset
_reset:
    la sp, _stack_top
    la t0, _bss_start
    la t1, _bss_end
1:
    bgeu t0, t1, 2f
#if __riscv_xlen == 64
    sd zero, 0(t0)
    addi t0, t0, 8
#else
    sw zero, 0(t0)
    addi t0, t0, 4
#endif
    j 1b
2:
    call lusa_entry
3:
    j 3b
