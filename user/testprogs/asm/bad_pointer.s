// console_write with a kernel pointer: must return -2 (Fault); exits with 9 if so, 10 otherwise.
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
