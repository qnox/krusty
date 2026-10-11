/* Threads for the drivers that test the mutator lock: what foreign code does when it starts a
   thread that calls into Kotlin. The runtime starts no thread of its own, and a driver has no C
   library, so this is `clone(2)` directly: a thread sharing the address space, files and signal
   handlers, on a stack the driver maps, whose id word the kernel clears and wakes when it exits.
   Include this from exactly one file per driver. */
#ifndef KRUSTY_DRIVER_THREADS_H
#define KRUSTY_DRIVER_THREADS_H

#include "krusty_rt.h"
#include "krusty_sys.h"

#define DRIVER_STACK_BYTES (256u * 1024u)

#if defined(__x86_64__)
#define KT_SYS_SCHED_YIELD 24
#else
#define KT_SYS_SCHED_YIELD 124
#endif

/* CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND | CLONE_THREAD | CLONE_SYSVSEM |
   CLONE_CHILD_CLEARTID: a thread as a C library makes one. */
#define DRIVER_CLONE_FLAGS 0x00250f00ul

/* Start `routine(argument)` on a new thread whose stack ends at `stack_top` (16-byte aligned). The
   kernel clears `*exited` and wakes it when the thread ends. Answers the new thread's id, or a
   negated errno. */
long driver_clone(unsigned long flags, void *stack_top, uint32_t *exited,
                  void (*routine)(void *), void *argument);

#if defined(__x86_64__)
/* clone(flags, stack, parent_tid, child_tid, tls): rdi, rsi, rdx, r10, r8. The routine and its
   argument ride on the new stack, where only the child pops them. */
__asm__(".text\n"
        ".globl driver_clone\n"
        "driver_clone:\n"
        "  subq $16, %rsi\n"
        "  movq %rcx, 0(%rsi)\n"
        "  movq %r8, 8(%rsi)\n"
        "  movq %rdx, %r10\n"
        "  xorl %edx, %edx\n"
        "  xorl %r8d, %r8d\n"
        "  movl $56, %eax\n"
        "  syscall\n"
        "  testq %rax, %rax\n"
        "  jnz 1f\n"
        "  xorl %ebp, %ebp\n"
        "  popq %rax\n"
        "  popq %rdi\n"
        "  call *%rax\n"
        "  movl $60, %eax\n"
        "  xorl %edi, %edi\n"
        "  syscall\n"
        "1:\n"
        "  ret\n");
#elif defined(__aarch64__)
/* clone(flags, stack, parent_tid, tls, child_tid): x0..x4. */
__asm__(".text\n"
        ".globl driver_clone\n"
        "driver_clone:\n"
        "  stp x3, x4, [x1, #-16]!\n"
        "  mov x4, x2\n"
        "  mov x2, #0\n"
        "  mov x3, #0\n"
        "  mov x8, #220\n"
        "  svc #0\n"
        "  cbnz x0, 1f\n"
        "  mov x29, #0\n"
        "  ldp x9, x0, [sp], #16\n"
        "  blr x9\n"
        "  mov x8, #93\n"
        "  mov x0, #0\n"
        "  svc #0\n"
        "1:\n"
        "  ret\n");
#elif defined(__riscv) && __riscv_xlen == 64
/* clone(flags, stack, parent_tid, tls, child_tid): a0..a4. */
__asm__(".text\n"
        ".globl driver_clone\n"
        "driver_clone:\n"
        "  addi a1, a1, -16\n"
        "  sd a3, 0(a1)\n"
        "  sd a4, 8(a1)\n"
        "  mv a4, a2\n"
        "  li a2, 0\n"
        "  li a3, 0\n"
        "  li a7, 220\n"
        "  ecall\n"
        "  bnez a0, 1f\n"
        "  li s0, 0\n"
        "  ld t0, 0(sp)\n"
        "  ld a0, 8(sp)\n"
        "  addi sp, sp, 16\n"
        "  jalr t0\n"
        "  li a7, 93\n"
        "  li a0, 0\n"
        "  ecall\n"
        "1:\n"
        "  ret\n");
#else
#error "krusty native: unsupported architecture"
#endif

typedef struct DriverThread {
    void *stack;
    uint32_t exited;
} DriverThread;

static inline void driver_thread_start(DriverThread *thread, void (*routine)(void *),
                                       void *argument) {
    thread->stack = kt_map(DRIVER_STACK_BYTES);
    __atomic_store_n(&thread->exited, 1, __ATOMIC_RELEASE);
    long started = driver_clone(DRIVER_CLONE_FLAGS, (char *)thread->stack + DRIVER_STACK_BYTES,
                                &thread->exited, routine, argument);
    if (started <= 0) {
        KT_SYS_FAIL("clone failed\n");
    }
}

/* Wait for the thread to end, with the lock released as any blocking foreign call has it, and give
   its stack back: from here on nothing may read it, which is what a collection after a join
   checks. The kernel's wake on exit is a shared futex wake, so the wait is a shared wait. */
static inline void driver_thread_join(DriverThread *thread) {
    KThread *self = kt_native_enter();
    while (__atomic_load_n(&thread->exited, __ATOMIC_ACQUIRE) != 0) {
        kt_syscall(KT_SYS_FUTEX, (long)&thread->exited, 0 /* FUTEX_WAIT */, 1, 0, 0, 0);
    }
    kt_native_leave(self);
    kt_unmap(thread->stack, DRIVER_STACK_BYTES);
    thread->stack = NULL;
}

#endif
