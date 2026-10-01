/* Isolated PIT/RTC/PIC delivery fixture. CODE, SCRATCH and CS are harness inputs.
 * Private IDT at SCRATCH, trace at SCRATCH+2048, count at SCRATCH+2560. */
.code32
.text
.global _start
_start:
    cli
    movl $CODE+512, %eax
    movw %ax, SCRATCH+256
    shrl $16, %eax
    movw %ax, SCRATCH+262
    movw $CS, SCRATCH+258
    movw $0x8e00, SCRATCH+260
    movl $CODE+544, %eax
    movw %ax, SCRATCH+320
    shrl $16, %eax
    movw %ax, SCRATCH+326
    movw $CS, SCRATCH+322
    movw $0x8e00, SCRATCH+324
    movw $2047, SCRATCH+2576
    movl $SCRATCH, SCRATCH+2578
    lidt SCRATCH+2576
    /* Remap PIC and admit PIT IRQ0 and RTC IRQ8. */
    movb $0x11, %al
    outb %al, $0x20
    outb %al, $0xa0
    movb $0x20, %al
    outb %al, $0x21
    movb $0x28, %al
    outb %al, $0xa1
    movb $4, %al
    outb %al, $0x21
    movb $2, %al
    outb %al, $0xa1
    movb $1, %al
    outb %al, $0x21
    outb %al, $0xa1
    movb $0xfe, %al
    outb %al, $0xa1
    movb $0xfa, %al
    outb %al, $0x21
    /* Mode 2, divisor 1193: approximately one millisecond. */
    movb $0x34, %al
    outb %al, $0x43
    movb $0xa9, %al
    outb %al, $0x40
    movb $4, %al
    outb %al, $0x40
    /* RTC periodic IRQ at 1024 Hz; clear any pre-existing status. */
    movb $0x8a, %al
    outb %al, $0x70
    movb $0x26, %al
    outb %al, $0x71
    movb $0x8b, %al
    outb %al, $0x70
    movb $0x42, %al
    outb %al, $0x71
    movb $0x8c, %al
    outb %al, $0x70
    inb $0x71, %al
    sti
work:
    .if IDLE
    hlt
    .endif
    incl SCRATCH+2564
    cmpl $32, SCRATCH+2560
    jb work
    cli
    jmp completed
    .org 256
completed:
    hlt
    jmp completed
    .org 512
irq0:
    pushal
    movl $0x20, %ebx
    jmp record
    .org 544
irq8:
    pushal
    movb $0x8c, %al
    outb %al, $0x70
    inb $0x71, %al
    movl $0x28, %ebx
record:
    movl SCRATCH+2560, %ecx
    cmpl $32, %ecx
    jae ack
    shll $4, %ecx
    movl %ebx, SCRATCH+2048(%ecx)
    rdtsc
    movl %eax, SCRATCH+2052(%ecx)
    movl %edx, SCRATCH+2056(%ecx)
    incl SCRATCH+2560
ack:
    movb $0x20, %al
    cmpl $0x28, %ebx
    jne master
    outb %al, $0xa0
master:
    outb %al, $0x20
    popal
    iret
