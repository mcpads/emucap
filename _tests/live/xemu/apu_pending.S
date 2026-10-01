/* Guest-owned fixture. APU BAR is read from PCI configuration; device methods
 * come from the pinned MCPX register definitions. SCRATCH is caller-provided. */
.code32
.text
.global _start
_start:
    cli
    movw $0xcf8, %dx
    movl $0x80002810, %eax
    outl %eax, %dx
    movw $0xcfc, %dx
    inl %dx, %eax
    andl $0xfffffff0, %eax
    movl %eax, SCRATCH
    movl %eax, %ebx
    xorl %ecx, %ecx
lock_voice:
    movl %ecx, 0x202f8(%ebx)
    movl $1, 0x202fc(%ebx)
    incl %ecx
    cmpl $256, %ecx
    jne lock_voice
    movl $2000000, %ecx
locked_work:
    decl %ecx
    jnz locked_work
    jmp pending
    .org 256
pending:
    xorl %ecx, %ecx
unlock_voice:
    movl %ecx, 0x202f8(%ebx)
    movl $0, 0x202fc(%ebx)
    incl %ecx
    cmpl $256, %ecx
    jne unlock_voice
    movl $2000000, %ecx
unlocked_work:
    decl %ecx
    jnz unlocked_work
    jmp completed
    .org 512
completed:
    hlt
    jmp completed
