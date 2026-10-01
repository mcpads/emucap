/* A privileged CPU stays halted while device timers continue. */
.code32
.text
.global _start
_start:
    cli
1:
    hlt
    jmp 1b
