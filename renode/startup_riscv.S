/* Lusa bare-metal startup for RISC-V boards (`virt`, ESP32-C3).
 *
 * Renode enters at the ELF entry symbol (`_reset`).  The generated Lusa C
 * translation unit exports the user's `#[entry]` function and the build
 * renames it to `lusa_entry`; initialise the stack pointer and BSS first,
 * then install the trap entry and call it.  This stub uses no libc.
 *
 * The build replaces the trap-entry marker below with one pointer per
 * `mcause` from 0 through the target's highest vector.  Timer and external
 * entries resolve to `lusa_isr_<vector>`; the linker provides
 * `_default_handler` fallbacks for handlers the program did not declare.
 */
.section .text.init, "ax"
.globl _reset
_reset:
    la sp, _stack_top

    /* Direct-mode trap entry: every exception and interrupt funnels through
     * `_trap_entry`, which dispatches on mcause.  Enable the machine software,
     * timer, and external interrupt classes; `mstatus.MIE` stays clear until
     * the program calls `interrupts_enable()`. */
    la t0, _trap_entry
    csrw mtvec, t0
    li t0, 0x888
    csrw mie, t0

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

/* Save the caller-saved registers, mask the interrupt bit from mcause, index
 * the handler table, and restore the interrupted context before mret. */
.section .text.init, "ax"
.align 4
.globl _trap_entry
_trap_entry:
#if __riscv_xlen == 64
    addi sp, sp, -128
    sd ra, 0(sp)
    sd t0, 8(sp)
    sd t1, 16(sp)
    sd t2, 24(sp)
    sd t3, 32(sp)
    sd t4, 40(sp)
    sd t5, 48(sp)
    sd t6, 56(sp)
    sd a0, 64(sp)
    sd a1, 72(sp)
    sd a2, 80(sp)
    sd a3, 88(sp)
    sd a4, 96(sp)
    sd a5, 104(sp)
    sd a6, 112(sp)
    sd a7, 120(sp)
#else
    addi sp, sp, -64
    sw ra, 0(sp)
    sw t0, 4(sp)
    sw t1, 8(sp)
    sw t2, 12(sp)
    sw t3, 16(sp)
    sw t4, 20(sp)
    sw t5, 24(sp)
    sw t6, 28(sp)
    sw a0, 32(sp)
    sw a1, 36(sp)
    sw a2, 40(sp)
    sw a3, 44(sp)
    sw a4, 48(sp)
    sw a5, 52(sp)
    sw a6, 56(sp)
    sw a7, 60(sp)
#endif
    csrr t0, mcause
    slli t0, t0, 1
    srli t0, t0, 1
    la t1, _trap_vectors
#if __riscv_xlen == 64
    slli t0, t0, 3
#else
    slli t0, t0, 2
#endif
    add t2, t1, t0
    la t3, _trap_vectors_end
    bgeu t2, t3, 1f
#if __riscv_xlen == 64
    ld t2, 0(t2)
#else
    lw t2, 0(t2)
#endif
    jalr ra, 0(t2)
    j 2f
1:
    call _default_handler
2:
#if __riscv_xlen == 64
    ld ra, 0(sp)
    ld t0, 8(sp)
    ld t1, 16(sp)
    ld t2, 24(sp)
    ld t3, 32(sp)
    ld t4, 40(sp)
    ld t5, 48(sp)
    ld t6, 56(sp)
    ld a0, 64(sp)
    ld a1, 72(sp)
    ld a2, 80(sp)
    ld a3, 88(sp)
    ld a4, 96(sp)
    ld a5, 104(sp)
    ld a6, 112(sp)
    ld a7, 120(sp)
    addi sp, sp, 128
#else
    lw ra, 0(sp)
    lw t0, 4(sp)
    lw t1, 8(sp)
    lw t2, 12(sp)
    lw t3, 16(sp)
    lw t4, 20(sp)
    lw t5, 24(sp)
    lw t6, 28(sp)
    lw a0, 32(sp)
    lw a1, 36(sp)
    lw a2, 40(sp)
    lw a3, 44(sp)
    lw a4, 48(sp)
    lw a5, 52(sp)
    lw a6, 56(sp)
    lw a7, 60(sp)
    addi sp, sp, 64
#endif
    mret

.section .rodata, "a"
.align 3
.globl _trap_vectors
_trap_vectors:
@LUSA_TRAP_ENTRIES@
_trap_vectors_end:

.section .text.init, "ax"
.align 2
.globl _default_handler
_default_handler:
    j _default_handler

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
