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

    /* Copy initialized data from its load image (flash on MCUs, RAM on the
     * `virt` boards, where load and virtual addresses are identical). */
    la t0, _data_load_start
    la t1, _data_start
    la t2, _data_end
1:
    bgeu t1, t2, 2f
#if __riscv_xlen == 64
    ld t3, 0(t0)
    sd t3, 0(t1)
    addi t0, t0, 8
    addi t1, t1, 8
#else
    lw t3, 0(t0)
    sw t3, 0(t1)
    addi t0, t0, 4
    addi t1, t1, 4
#endif
    j 1b

    /* Zero .bss. */
2:
    la t0, _bss_start
    la t1, _bss_end
3:
    bgeu t0, t1, 4f
#if __riscv_xlen == 64
    sd zero, 0(t0)
    addi t0, t0, 8
#else
    sw zero, 0(t0)
    addi t0, t0, 4
#endif
    j 3b
4:
    call lusa_entry
5:
    j 5b

/* GCC can lower aggregate copies and zeroing to libc calls even with
 * -ffreestanding.  Supply byte-wise implementations for freestanding images. */
.section .text, "ax"
.globl memcpy
memcpy:
    mv t0, a0
1:
    beqz a2, 2f
    lbu t1, 0(a1)
    sb t1, 0(t0)
    addi a1, a1, 1
    addi t0, t0, 1
    addi a2, a2, -1
    j 1b
2:
    ret

.globl memset
memset:
    mv t0, a0
1:
    beqz a2, 2f
    sb a1, 0(t0)
    addi t0, t0, 1
    addi a2, a2, -1
    j 1b
2:
    ret
