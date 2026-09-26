// Syscall inexistente: debe devolver -1 (NoSys); sale con 7 si es asi, 8 si no.
.section .text._start, "ax"
.global _start
_start:
    mov eax, 999
    syscall
    cmp rax, -1
    jne 2f
    mov edi, 7
    jmp 3f
2:  mov edi, 8
3:  mov eax, 0
    syscall
