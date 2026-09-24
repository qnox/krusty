/* krusty native runtime: the process entry point. Hand-written freestanding C; build.rs compiles it
   into the runtime for every target. */
#include "krusty_rt.h"
#include "krusty_sys.h"

void kt_program_entry(void);

/* Where `_start` goes when the program returns: the process ends with status 0, as a Kotlin `main`
   that returns does. Without it the instruction after the call would trap, reporting a normal end as
   a crash. */
__attribute__((noreturn, used)) void kt_program_returned(void) { kt_sys_exit(0); }

#if defined(__x86_64__)
__asm__(".globl _start\n"
        "_start:\n"
        "  xorl %ebp, %ebp\n"
        "  andq $-16, %rsp\n"
        "  call kt_program_entry\n"
        "  call kt_program_returned\n");
#elif defined(__aarch64__)
__asm__(".globl _start\n"
        "_start:\n"
        "  mov x29, #0\n"
        "  mov x30, #0\n"
        "  bl kt_program_entry\n"
        "  bl kt_program_returned\n");
#elif defined(__riscv) && __riscv_xlen == 64
__asm__(".globl _start\n"
        "_start:\n"
        "  li s0, 0\n"
        "  li ra, 0\n"
        "  call kt_program_entry\n"
        "  call kt_program_returned\n");
#else
#error "krusty native: unsupported architecture"
#endif
