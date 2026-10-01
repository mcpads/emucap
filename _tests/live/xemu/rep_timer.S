/* SCRATCH and ITERATIONS are supplied to the assembler by the runtime harness.
 * IRQ delivery is masked; native device timers continue while guest work runs. */
.code32
.text
.global _start
_start:
    cli
    cld
    rdtsc
    movl %eax, SCRATCH+512
    movl %edx, SCRATCH+516
    movl $SCRATCH, %edi
    movl $64, %ecx
    movl $0x13579bdf, %eax
    rep stosl
    movl $SCRATCH, %esi
    movl $SCRATCH+256, %edi
    movl $64, %ecx
    rep movsl
    movl $ITERATIONS, %ecx
1:
    decl %ecx
    jnz 1b
    rdtsc
    movl %eax, SCRATCH+520
    movl %edx, SCRATCH+524
    jmp completed
    .org 256
completed:
    hlt
    jmp completed
