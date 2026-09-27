#!/usr/bin/env python3
"""Genera kernel/src/arch/x86_64/stubs.s: 256 stubs de interrupción + tabla."""
ERR = {8, 10, 11, 12, 13, 14, 17, 21, 29, 30}
out = [
    "// Generado por tools/gen-stubs.py; no editar a mano.",
    "// Cada stub deja la pila como [vector][error_code][rip][cs][rflags][rsp][ss]",
    "// y salta al trampolín común, que guarda los registros generales, hace",
    "// swapgs si venía de ring 3 (cs en [rsp+144] tras los 15 registros) y",
    "// llama a interrupt_dispatch(&mut InterruptFrame).",
    ".section .text.interrupt_stubs, \"ax\"",
    ".code64",
    "",
]
for v in range(256):
    out.append(f"interrupt_stub_{v}:")
    if v not in ERR:
        out.append("    push 0")
    out.append(f"    push {v}")
    out.append("    jmp interrupt_common")
for v in range(256):
    pass
out += [
    "",
    "interrupt_common:",
    "    push rax", "    push rcx", "    push rdx", "    push rbx", "    push rbp",
    "    push rsi", "    push rdi", "    push r8", "    push r9", "    push r10",
    "    push r11", "    push r12", "    push r13", "    push r14", "    push r15",
    "    // Desde ring 3 (cs & 3): intercambiar GS para ver el Cpu de esta CPU.",
    "    test qword ptr [rsp + 144], 3",
    "    jz 1f",
    "    swapgs",
    "1:",
    "    mov rdi, rsp",
    "    cld",
    "    call interrupt_dispatch",
    "    test qword ptr [rsp + 144], 3",
    "    jz 2f",
    "    swapgs",
    "2:",
    "    pop r15", "    pop r14", "    pop r13", "    pop r12", "    pop r11",
    "    pop r10", "    pop r9", "    pop r8", "    pop rdi", "    pop rsi",
    "    pop rbp", "    pop rbx", "    pop rdx", "    pop rcx", "    pop rax",
    "    add rsp, 16",
    "    iretq",
    "",
    ".section .rodata.interrupt_stub_table, \"a\"",
    ".balign 8",
    ".global interrupt_stub_table",
    "interrupt_stub_table:",
]
out += [f"    .quad interrupt_stub_{v}" for v in range(256)]
open("kernel/src/arch/x86_64/stubs.s", "w").write("\n".join(out) + "\n")
