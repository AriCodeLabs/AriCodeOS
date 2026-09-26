// Programas de usuario de prueba (Fase 11). Codigo independiente de posicion,
// copiado a una pagina de usuario en 0x400000 y ejecutado en ring 3.
// ABI: rax = numero, rdi/rsi/rdx = args, rax = resultado (ver sdk/abi).
//   0 process_exit(code)  1 console_write(ptr,len)  2 thread_yield()
//   3 thread_sleep(ms)    4 thread_get_tid()        5 abi_version()
.section .rodata.user_programs, "a"

.global user_prog_hello_start, user_prog_hello_end
user_prog_hello_start:
    mov eax, 1
    lea rdi, [rip + 1f]
    mov esi, 18
    syscall
    mov eax, 0
    mov edi, 42
    syscall
    ud2
1:  .ascii "hola desde ring 3\n"
user_prog_hello_end:

.global user_prog_kernel_read_start, user_prog_kernel_read_end
user_prog_kernel_read_start:
    mov rax, 0xffffffff80000000
    mov rax, [rax]
    mov eax, 0
    mov edi, 1
    syscall
user_prog_kernel_read_end:

.global user_prog_kernel_write_start, user_prog_kernel_write_end
user_prog_kernel_write_start:
    mov rax, 0xffffffff80005000
    mov qword ptr [rax], 1
    mov eax, 0
    mov edi, 1
    syscall
user_prog_kernel_write_end:

.global user_prog_privileged_start, user_prog_privileged_end
user_prog_privileged_start:
    hlt
    mov eax, 0
    mov edi, 1
    syscall
user_prog_privileged_end:

// Syscall inexistente: debe devolver -1 (NoSys); sale con 7 si es asi, 8 si no.
.global user_prog_bad_syscall_start, user_prog_bad_syscall_end
user_prog_bad_syscall_start:
    mov eax, 999
    syscall
    cmp rax, -1
    jne 2f
    mov edi, 7
    jmp 3f
2:  mov edi, 8
3:  mov eax, 0
    syscall
user_prog_bad_syscall_end:

// console_write con puntero de kernel: debe devolver -2 (Fault); sale con 9 si es asi, 10 si no.
.global user_prog_bad_pointer_start, user_prog_bad_pointer_end
user_prog_bad_pointer_start:
    mov eax, 1
    mov rdi, 0xffffffff80000000
    mov esi, 16
    syscall
    cmp rax, -2
    jne 2f
    mov edi, 9
    jmp 3f
2:  mov edi, 10
3:  mov eax, 0
    syscall
user_prog_bad_pointer_end:

// sleep, yield, get_tid y abi_version; sale con 3 si todo cuadra.
.global user_prog_sleep_yield_start, user_prog_sleep_yield_end
user_prog_sleep_yield_start:
    mov eax, 3
    mov edi, 20
    syscall
    mov eax, 2
    syscall
    mov eax, 4
    syscall
    mov r12, rax        // tid (> 1: main es 0, idle es 1)
    mov eax, 5
    syscall
    cmp rax, 1          // ABI_VERSION
    jne 2f
    cmp r12, 1
    jbe 2f
    mov edi, 3
    jmp 3f
2:  mov edi, 4
3:  mov eax, 0
    syscall
user_prog_sleep_yield_end:
