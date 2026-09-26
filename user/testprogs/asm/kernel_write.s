.section .text._start, "ax"
.global _start
_start:
    mov rax, 0xffffffff80005000
    mov qword ptr [rax], 1
    mov eax, 0
    mov edi, 1
    syscall
