// Nonexistent syscall: must return -1 (NoSys); exits with 7 if so, 8 otherwise.
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
