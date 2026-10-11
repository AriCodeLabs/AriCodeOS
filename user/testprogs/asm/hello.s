.section .text._start, "ax"
.global _start
_start:
    mov eax, 1
    mov edi, 0x10003
    lea rsi, [rip + 1f]
    mov edx, 18
    syscall
    mov eax, 0
    mov edi, 42
    syscall
    ud2
1:  .ascii "hello from ring 3\n"
