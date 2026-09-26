.section .text._start, "ax"
.global _start
_start:
    hlt
    mov eax, 0
    mov edi, 1
    syscall
