// Programas de usuario de prueba (Fase 11). Codigo independiente de posicion,
// copiado a una pagina de usuario en 0x400000 y ejecutado en ring 3.
// ABI: rax = numero, rdi/rsi/rdx = args, rax = resultado (ver sdk/abi).
//   0 process_exit(code)  1 console_write(handle,ptr,len)  2 thread_yield()
//   3 thread_sleep(ms)    4 thread_get_tid()  5 abi_version()
//   6 handle_close(h)     7 handle_duplicate(h,rights)     8 object_kind(h)
//   9 object_id(h)
// Handles iniciales: 0x10001 proceso, 0x10002 hilo, 0x10003 consola.
.section .rodata.user_programs, "a"

.global user_prog_hello_start, user_prog_hello_end
user_prog_hello_start:
    mov eax, 1
    mov edi, 0x10003
    lea rsi, [rip + 1f]
    mov edx, 18
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
    cmp rax, 2          // ABI_VERSION
    jne 2f
    cmp r12, 1
    jbe 2f
    mov edi, 3
    jmp 3f
2:  mov edi, 4
3:  mov eax, 0
    syscall
user_prog_sleep_yield_end:

// Handles: cada paso que falla sale con su numero; exito = 11.
.global user_prog_handles_start, user_prog_handles_end
user_prog_handles_start:
    // 1. object_kind(consola) == 3
    mov eax, 8
    mov edi, 0x10003
    syscall
    cmp rax, 3
    jne 91f
    // 2. handle inventado -> BadHandle (-4)
    mov eax, 8
    mov edi, 0x1234
    syscall
    cmp rax, -4
    jne 92f
    // 3. duplicar consola sin derechos (rights 0)
    mov eax, 7
    mov edi, 0x10003
    xor esi, esi
    syscall
    test rax, rax
    js 93f
    mov r12, rax        // consola solo lectura
    // 4. escribir con el duplicado -> Denied (-5)
    mov eax, 1
    mov rdi, r12
    lea rsi, [rip + 1f]
    mov edx, 3
    syscall
    cmp rax, -5
    jne 94f
    // 5. escalar derechos: duplicar el duplicado pidiendo WRITE -> Denied (sin DUPLICATE)
    mov eax, 7
    mov rdi, r12
    mov esi, 2
    syscall
    cmp rax, -5
    jne 95f
    // 6. escalar desde la consola pidiendo MANAGE -> Escalation (-6)
    mov eax, 7
    mov edi, 0x10003
    mov esi, 0x40
    syscall
    cmp rax, -6
    jne 96f
    // 7. cerrar la consola original y escribir -> BadHandle (-4)
    mov eax, 6
    mov edi, 0x10003
    syscall
    test rax, rax
    jnz 97f
    mov eax, 1
    mov edi, 0x10003
    lea rsi, [rip + 1f]
    mov edx, 3
    syscall
    cmp rax, -4
    jne 97f
    // 8. el hilo propio no es la consola: escribir con el -> Denied (sin WRITE)
    mov eax, 1
    mov edi, 0x10002
    lea rsi, [rip + 1f]
    mov edx, 3
    syscall
    cmp rax, -5
    jne 98f
    // 9. object_id(hilo) == thread_get_tid()
    mov eax, 4
    syscall
    mov r13, rax
    mov eax, 9
    mov edi, 0x10002
    syscall
    cmp rax, r13
    jne 90f
    mov edi, 11
    jmp 99f
90: mov edi, 9
    jmp 99f
91: mov edi, 1
    jmp 99f
92: mov edi, 2
    jmp 99f
93: mov edi, 3
    jmp 99f
94: mov edi, 4
    jmp 99f
95: mov edi, 5
    jmp 99f
96: mov edi, 6
    jmp 99f
97: mov edi, 7
    jmp 99f
98: mov edi, 8
99: mov eax, 0
    syscall
1:  .ascii "ok\n"
user_prog_handles_end:
