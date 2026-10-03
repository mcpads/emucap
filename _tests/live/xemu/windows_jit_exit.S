.text
.globl test_jump_restore
.def test_jump_restore; .scl 2; .type 32; .endef
test_jump_restore:
.irp reg,rbx,rbp,rsi,rdi,r12,r13,r14,r15
 pushq %\reg
.endr
 subq $456,%rsp
 movq %rcx,448(%rsp)
.set slot,288
.irp reg,xmm6,xmm7,xmm8,xmm9,xmm10,xmm11,xmm12,xmm13,xmm14,xmm15
 movdqu %\reg,slot(%rsp)
.set slot,slot+16
.endr
.irp reg,rbx,rbp,rsi,rdi,r12,r13,r14,r15
 movabsq $0x123456789abcdef0,%\reg
.endr
.irp reg,xmm6,xmm7,xmm8,xmm9,xmm10,xmm11,xmm12,xmm13,xmm14,xmm15
 movdqu pattern(%rip),%\reg
.endr
 leaq 32(%rsp),%rcx
 xorq %rdx,%rdx
 call *saved_setjmp(%rip)
 testl %eax,%eax
 jnz verify
 leaq 32(%rsp),%rcx
 movl test_jump_value(%rip),%edx
 call *448(%rsp)
 jmp failed
verify:
 movl test_jump_value(%rip),%edx
 testl %edx,%edx
 jnz 1f
 incl %edx
1:
 cmpl %edx,%eax
 jne failed
 movabsq $0x123456789abcdef0,%rax
.irp reg,rbx,rbp,rsi,rdi,r12,r13,r14,r15
 cmpq %rax,%\reg
 jne failed
.endr
.irp reg,xmm6,xmm7,xmm8,xmm9,xmm10,xmm11,xmm12,xmm13,xmm14,xmm15
 movdqa %\reg,%xmm0
 pcmpeqb pattern(%rip),%xmm0
 pmovmskb %xmm0,%eax
 cmpl $65535,%eax
 jne failed
.endr
 xorl %edx,%edx
 jmp restore
failed:
 movl $1,%edx
restore:
.set slot,288
.irp reg,xmm6,xmm7,xmm8,xmm9,xmm10,xmm11,xmm12,xmm13,xmm14,xmm15
 movdqu slot(%rsp),%\reg
.set slot,slot+16
.endr
 addq $456,%rsp
.irp reg,r15,r14,r13,r12,rdi,rsi,rbp,rbx
 popq %\reg
.endr
 movl %edx,%eax
 ret
.section .rdata,"dr"
.p2align 4
pattern:
 .quad 0x123456789abcdef0,0xfedcba9876543210
