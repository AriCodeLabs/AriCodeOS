// console_write con puntero de kernel: debe devolver -2 (Fault); sale con 9 si es asi, 10 si no.
.section .text._start, "ax"
.global _start
_start:
    mov eax, 1
    mov edi, 0x10003
    mov rsi, 0xffffffff80000000
    mov edx, 16
    syscall
    cmp rax, -2
    jne 2f
    mov edi, 9
    jmp 3f
2:  mov edi, 10
3:  mov eax, 0
    syscall
