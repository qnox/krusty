/* krusty native runtime — generated; do not edit. */
#include "krusty_rt.h"

void kt_program_entry(void);

#if defined(__x86_64__)
__asm__(".globl _start\n"
        "_start:\n"
        "  xorl %ebp, %ebp\n"
        "  andq $-16, %rsp\n"
        "  call kt_program_entry\n"
        "  hlt\n");
#elif defined(__aarch64__)
__asm__(".globl _start\n"
        "_start:\n"
        "  mov x29, #0\n"
        "  mov x30, #0\n"
        "  bl kt_program_entry\n"
        "  brk #0\n");
#elif defined(__riscv) && __riscv_xlen == 64
__asm__(".globl _start\n"
        "_start:\n"
        "  li s0, 0\n"
        "  li ra, 0\n"
        "  call kt_program_entry\n"
        "  ebreak\n");
#else
#error "krusty native: unsupported architecture"
#endif
