.section .text._start, "ax"
.global _start
_start:
    mov rax, 0xffffffff80000000
    mov rax, [rax]
    mov eax, 0
    mov edi, 1
    syscall
