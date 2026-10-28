/* Lusa bare-metal startup for Arm Cortex-M boards (RP2040, STM32F4).
 *
 * The linker script places `_vectors` first in flash.  Renode loads the ELF
 * and starts at `_reset`; on real hardware the vector table supplies the
 * initial stack pointer and reset address.  Both paths converge on `_reset`,
 * which sets VTOR, copies `.data` from flash to SRAM, zeroes `.bss`, calls the
 * generated `lusa_entry`, and parks the core.
 *
 * The table contains the 16 architectural Cortex-M exceptions.  Boards with
 * external interrupts should extend it with their device vectors.
 */
.syntax unified
.thumb

.section .vectors, "a", %progbits
.align 2
.globl _vectors
_vectors:
    .word _stack_top
    .word _reset
    .rept 14
    .word _default_handler
    .endr
.size _vectors, . - _vectors

.section .text.init, "ax", %progbits
.align 2
.globl _reset
.thumb_func
.type _reset, %function
_reset:
    cpsid i
    ldr r0, =_stack_top
    mov sp, r0

    /* Route exceptions through this image's vector table. */
    ldr r0, =0xE000ED08
    ldr r1, =_vectors
    str r1, [r0]

    /* Copy initialized data from its flash load image into SRAM. */
    ldr r0, =_data_load_start
    ldr r1, =_data_start
    ldr r2, =_data_end
1:
    cmp r1, r2
    bhs 2f
    ldr r3, [r0]
    str r3, [r1]
    adds r0, r0, #4
    adds r1, r1, #4
    b 1b

    /* Zero .bss. */
2:
    ldr r1, =_bss_start
    ldr r2, =_bss_end
    movs r3, #0
3:
    cmp r1, r2
    bhs 4f
    str r3, [r1]
    adds r1, r1, #4
    b 3b

4:
    bl lusa_entry
5:
    b 5b
.size _reset, . - _reset

.section .text.init, "ax", %progbits
.align 2
.globl _default_handler
.thumb_func
.type _default_handler, %function
_default_handler:
    b _default_handler
.size _default_handler, . - _default_handler

/* GCC may lower aggregate copies and zeroing to libc calls even with
 * -ffreestanding.  Provide the two routines a freestanding image can need.
 * These are byte-wise and intentionally simple. */
.section .text, "ax", %progbits
.align 2
.globl memcpy
.thumb_func
.type memcpy, %function
memcpy:
    push {r4, lr}
    mov r4, r0
    mov r3, r0
1:
    cmp r2, #0
    beq 2f
    ldrb r0, [r1]
    strb r0, [r3]
    adds r1, r1, #1
    adds r3, r3, #1
    subs r2, r2, #1
    b 1b
2:
    mov r0, r4
    pop {r4, pc}
.size memcpy, . - memcpy

.align 2
.globl memset
.thumb_func
.type memset, %function
memset:
    push {r4, lr}
    mov r4, r0
    mov r3, r0
1:
    cmp r2, #0
    beq 2f
    strb r1, [r3]
    adds r3, r3, #1
    subs r2, r2, #1
    b 1b
2:
    mov r0, r4
    pop {r4, pc}
.size memset, . - memset
