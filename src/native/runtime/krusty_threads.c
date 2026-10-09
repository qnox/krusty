/* krusty native runtime: the threads that run Kotlin, and the mutator lock they share. Hand-written
   freestanding C; build.rs compiles it into the runtime for every target.

   A thread runs Kotlin either because the runtime started it (`kt_thread_start`, Go's way: the
   kernel's `clone`, no C library) or because foreign code did and called into Kotlin (a
   `pthread_create` start routine, when a program links a C library). Either way it shares one heap
   with every other. What keeps that heap and the rest of the runtime's state consistent is one
   lock: a thread HOLDS it while it runs Kotlin code or the runtime, and RELEASES it while it is in
   foreign code, which is where a thread blocks (a read, an accept, a wait on a mutex). So at most
   one thread mutates the heap at a time, and every other thread is parked at a known point with a
   known stack.

   That is what lets the collector stay a stop-the-world mark-sweep with no safepoints: a thread
   that collects holds the lock, so every other attached thread is released, and each one recorded
   where its Kotlin frames end and what its callee-saved registers held when it let go. The roots of
   a collection are the collector's own stack and registers, as before, plus each released thread's
   recorded registers and the stack between its recorded pointer and its bottom.

   The one slot generated code reads after every call, `kt_pending`, stays a plain global for the
   same reason: only the holder runs Kotlin, so the slot is the holder's, and it is saved into the
   thread on release and restored on acquisition.

   Two ways in and out, each a pair:

   * `kt_native_enter` / `kt_native_leave` bracket a call from Kotlin into foreign code. The thread
     stays attached and its Kotlin frames stay roots while it is away.
   * `kt_callback_enter` / `kt_callback_leave` bracket a call from foreign code into Kotlin. A
     thread the runtime has not seen is attached for the duration; a thread already in a
     `kt_native_enter` (a callback from the C function it called) re-enters as itself. */
#include "krusty_internal.h"

/* ---- the lock ----------------------------------------------------------------------------------

   A futex mutex: 0 free, 1 held, 2 held with waiters (Drepper, "Futexes Are Tricky", mutex 2). The
   futex is a system call, not libc, so the lock is the same whether or not the program links a C
   library. */

static uint32_t kt_mutator_lock;

static void kt_lock(void) {
    uint32_t state = 0;
    if (__atomic_compare_exchange_n(&kt_mutator_lock, &state, 1, false, __ATOMIC_ACQUIRE,
                                    __ATOMIC_RELAXED)) {
        return;
    }
    if (state != 2) {
        state = __atomic_exchange_n(&kt_mutator_lock, 2, __ATOMIC_ACQUIRE);
    }
    while (state != 0) {
        kt_sys_futex_wait(&kt_mutator_lock, 2);
        state = __atomic_exchange_n(&kt_mutator_lock, 2, __ATOMIC_ACQUIRE);
    }
}

static void kt_unlock(void) {
    if (__atomic_fetch_sub(&kt_mutator_lock, 1, __ATOMIC_RELEASE) != 1) {
        __atomic_store_n(&kt_mutator_lock, 0, __ATOMIC_RELEASE);
        kt_sys_futex_wake(&kt_mutator_lock, 1);
    }
}

/* ---- the registry ------------------------------------------------------------------------------

   Every attached thread, changed only by the holder. */

static KThread *kt_threads;

/* The holder, or NULL while every attached thread is in foreign code. Read by the release path in
   assembly, which is why it has external linkage. */
KThread *kt_running;

/* A record lives in its own mapping: a thread can be detached while others run, and the heap is for
   Kotlin values, not for the runtime's bookkeeping. */
static KThread *kt_thread_new(uintptr_t stack_bottom, long tid) {
    KThread *thread = (KThread *)kt_map(sizeof(KThread));
    thread->stack_bottom = stack_bottom;
    thread->tid = tid;
    thread->next = kt_threads;
    kt_threads = thread;
    return thread;
}

static void kt_thread_remove(KThread *thread) {
    KThread **link = &kt_threads;
    while (*link != thread) {
        link = &(*link)->next;
    }
    *link = thread->next;
    kt_unmap(thread, sizeof(KThread));
}

/* A thread's number in its name, `Thread-<n>`: the threads the runtime starts and those foreign code
   attaches are numbered from 0 in the order they appear, as the JVM numbers unnamed threads. The
   program's first thread is `main`. */
#define KT_MAIN_THREAD UINT32_MAX
static uint32_t kt_numbered_threads;

/* End the process on the exception `thread` left in flight, if it left one, reported on the
   thread's name as Kotlin reports an exception nothing caught. The caller holds the lock. */
static void kt_end_on_uncaught(const KThread *thread) {
    if (thread->number == KT_MAIN_THREAD) {
        kt_report_uncaught("main", 4);
        return;
    }
    char name[20] = "Thread-";
    size_t length = 7;
    char digits[10];
    size_t count = 0;
    uint32_t number = thread->number;
    do {
        digits[count++] = (char)('0' + number % 10);
        number /= 10;
    } while (number != 0);
    while (count > 0) {
        name[length++] = digits[--count];
    }
    kt_report_uncaught(name, length);
}

static KThread *kt_thread_of(long tid) {
    for (KThread *thread = kt_threads; thread != NULL; thread = thread->next) {
        if (thread->tid == tid) {
            return thread;
        }
    }
    return NULL;
}

/* Become the holder: take the lock and the thread's own exception slot. */
static void kt_acquire(KThread *thread) {
    kt_lock();
    kt_running = thread;
    thread->released = false;
    kt_pending = thread->pending;
    thread->pending = NULL;
}

/* Stop being the holder. The thread's registers and stack pointer are already recorded. */
static void kt_release(KThread *thread) {
    thread->pending = kt_pending;
    kt_pending = NULL;
    thread->released = true;
    kt_running = NULL;
    kt_unlock();
}

void kt_runtime_init(void *stack_bottom) {
    /* The thread that starts the program is the first attached thread, and the holder from here on.
       A second call (a driver that initializes twice) only moves its bottom. */
    if (kt_running != NULL) {
        kt_running->stack_bottom = (uintptr_t)stack_bottom;
        return;
    }
    kt_lock();
    kt_running = kt_thread_new((uintptr_t)stack_bottom, kt_sys_gettid());
    kt_running->number = KT_MAIN_THREAD;
}

/* ---- out to foreign code -----------------------------------------------------------------------

   `kt_native_enter` must record the callee-saved registers EXACTLY as its caller left them: a
   reference whose only copy is in one of them belongs to a Kotlin frame that is waiting for the
   foreign call to return, and the foreign code may keep it in that register for as long as it
   runs. A C function cannot promise that — its own prologue may already have reused a register
   before any statement runs — so the entry is assembly, which stores the registers into the
   holder's record, stores the stack pointer, and tail-calls the C half. Its offsets are the
   record's, asserted below. */

_Static_assert(offsetof(KThread, registers) == 0, "kt_native_enter stores registers at offset 0");
_Static_assert(offsetof(KThread, saved_sp) == 8 * KT_SAVED_REGISTERS,
               "kt_native_enter stores the stack pointer after the registers");

KThread *kt_native_release(void);

#if defined(__x86_64__)
__asm__(".text\n"
        ".globl kt_native_enter\n"
        ".type kt_native_enter, @function\n"
        "kt_native_enter:\n"
        "  movq kt_running(%rip), %rax\n"
        "  movq %rbx, 0(%rax)\n"
        "  movq %rbp, 8(%rax)\n"
        "  movq %r12, 16(%rax)\n"
        "  movq %r13, 24(%rax)\n"
        "  movq %r14, 32(%rax)\n"
        "  movq %r15, 40(%rax)\n"
        "  movq %rsp, 96(%rax)\n"
        "  jmp kt_native_release\n"
        ".size kt_native_enter, .-kt_native_enter\n");
#elif defined(__aarch64__)
__asm__(".text\n"
        ".globl kt_native_enter\n"
        ".type kt_native_enter, %function\n"
        "kt_native_enter:\n"
        "  adrp x9, kt_running\n"
        "  ldr x9, [x9, :lo12:kt_running]\n"
        "  stp x19, x20, [x9, #0]\n"
        "  stp x21, x22, [x9, #16]\n"
        "  stp x23, x24, [x9, #32]\n"
        "  stp x25, x26, [x9, #48]\n"
        "  stp x27, x28, [x9, #64]\n"
        "  str x29, [x9, #80]\n"
        "  mov x10, sp\n"
        "  str x10, [x9, #96]\n"
        "  b kt_native_release\n"
        ".size kt_native_enter, .-kt_native_enter\n");
#elif defined(__riscv) && __riscv_xlen == 64
__asm__(".text\n"
        ".globl kt_native_enter\n"
        ".type kt_native_enter, @function\n"
        "kt_native_enter:\n"
        "  lla t0, kt_running\n"
        "  ld t0, 0(t0)\n"
        "  sd s0, 0(t0)\n"
        "  sd s1, 8(t0)\n"
        "  sd s2, 16(t0)\n"
        "  sd s3, 24(t0)\n"
        "  sd s4, 32(t0)\n"
        "  sd s5, 40(t0)\n"
        "  sd s6, 48(t0)\n"
        "  sd s7, 56(t0)\n"
        "  sd s8, 64(t0)\n"
        "  sd s9, 72(t0)\n"
        "  sd s10, 80(t0)\n"
        "  sd s11, 88(t0)\n"
        "  sd sp, 96(t0)\n"
        "  tail kt_native_release\n"
        ".size kt_native_enter, .-kt_native_enter\n");
#else
#error "krusty native: unsupported architecture"
#endif

/* The C half of `kt_native_enter`, entered by a tail call with the caller's return address still in
   place, so its return is the entry's. The stack pointer the entry recorded is the caller's at the
   call: every Kotlin frame of this thread lies at or above it. */
KThread *kt_native_release(void) {
    KThread *thread = kt_running;
    kt_release(thread);
    return thread;
}

void kt_native_leave(KThread *thread) { kt_acquire(thread); }

/* ---- in from foreign code ----------------------------------------------------------------------- */

void kt_callback_enter(KThreadEntry *entry, void *stack_bottom) {
    long tid = kt_sys_gettid();
    kt_lock();
    KThread *thread = kt_thread_of(tid);
    entry->attached = thread == NULL;
    if (thread == NULL) {
        thread = kt_thread_new((uintptr_t)stack_bottom, tid);
        thread->number = kt_numbered_threads++;
    } else {
        /* A callback on a thread that is in a `kt_native_enter`: its released state describes the
           Kotlin frames waiting above this one, and is put back on the way out. Until then this
           thread's frames are scanned from wherever it is, which covers those too. */
        for (unsigned i = 0; i < KT_SAVED_REGISTERS; i++) {
            entry->outer_registers[i] = thread->registers[i];
        }
        entry->outer_sp = thread->saved_sp;
        entry->outer_pending = thread->pending;
        thread->pending = NULL;
    }
    entry->thread = thread;
    kt_running = thread;
    thread->released = false;
    kt_pending = NULL;
}

void kt_callback_leave(KThreadEntry *entry) {
    KThread *thread = entry->thread;
    /* An exception the callback did not catch has nowhere to go: the foreign code it would return to
       knows nothing of Kotlin exceptions, and once the lock is released a heap reference handed to it
       would be held by no root while another thread collects. So it ends the process here, with the
       lock still held, as Kotlin/Native ends one that escapes a `staticCFunction`. */
    kt_end_on_uncaught(thread);
    if (entry->attached) {
        /* The thread leaves the runtime: its stack is about to be returned to whoever owns it, and
           nothing on it is a root any more. */
        kt_thread_remove(thread);
        kt_running = NULL;
        kt_unlock();
        return;
    }
    /* Back to the foreign code that called in, and through it to the Kotlin frames that called out:
       the state recorded when they did is what describes this thread again. */
    for (unsigned i = 0; i < KT_SAVED_REGISTERS; i++) {
        thread->registers[i] = entry->outer_registers[i];
    }
    thread->saved_sp = entry->outer_sp;
    kt_pending = entry->outer_pending;
    kt_release(thread);
}

/* ---- threads the runtime starts ----------------------------------------------------------------

   The one platform-specific step is creating the kernel thread: here `clone(2)` on a stack the
   runtime maps, as Go does. A target that must start threads through a C library replaces
   `kt_spawn` with `pthread_create`; everything around it is the same. */

#define KT_THREAD_STACK_BYTES ((size_t)8 * 1024 * 1024)
#define KT_GUARD_BYTES ((size_t)4096)

/* CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND | CLONE_THREAD | CLONE_SYSVSEM |
   CLONE_CHILD_CLEARTID: a thread of this process, whose id word the kernel clears and wakes when it
   ends. */
#define KT_CLONE_FLAGS 0x00250f00ul

struct KThreadHandle {
    uint32_t exited;
    void *stack;
};

/* Start `routine(argument)` on a new thread whose stack ends at `stack_top`; the kernel clears
   `*exited` and wakes it when the thread ends. Answers the thread's id or a negated errno. The
   routine and argument ride on the new stack, where only the child reads them. */
long kt_clone(unsigned long flags, void *stack_top, uint32_t *exited, void (*routine)(void *),
              void *argument);

#if defined(__x86_64__)
/* clone(flags, stack, parent_tid, child_tid, tls): rdi, rsi, rdx, r10, r8. */
__asm__(".text\n"
        ".globl kt_clone\n"
        ".type kt_clone, @function\n"
        "kt_clone:\n"
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
        "  ud2\n"
        "1:\n"
        "  ret\n"
        ".size kt_clone, .-kt_clone\n");
#elif defined(__aarch64__)
/* clone(flags, stack, parent_tid, tls, child_tid): x0..x4. */
__asm__(".text\n"
        ".globl kt_clone\n"
        ".type kt_clone, %function\n"
        "kt_clone:\n"
        "  stp x3, x4, [x1, #-16]!\n"
        "  mov x4, x2\n"
        "  mov x2, #0\n"
        "  mov x3, #0\n"
        "  mov x8, #220\n"
        "  svc #0\n"
        "  cbnz x0, 1f\n"
        "  mov x29, #0\n"
        "  mov x30, #0\n"
        "  ldp x9, x0, [sp], #16\n"
        "  blr x9\n"
        "  brk #0\n"
        "1:\n"
        "  ret\n"
        ".size kt_clone, .-kt_clone\n");
#elif defined(__riscv) && __riscv_xlen == 64
/* clone(flags, stack, parent_tid, tls, child_tid): a0..a4. */
__asm__(".text\n"
        ".globl kt_clone\n"
        ".type kt_clone, @function\n"
        "kt_clone:\n"
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
        "  li ra, 0\n"
        "  ld t0, 0(sp)\n"
        "  ld a0, 8(sp)\n"
        "  addi sp, sp, 16\n"
        "  jalr t0\n"
        "  unimp\n"
        "1:\n"
        "  ret\n"
        ".size kt_clone, .-kt_clone\n");
#else
#error "krusty native: unsupported architecture"
#endif

/* What a started thread runs first: become the holder as itself, run the routine, and leave. It
   never returns: the thread ends here, after leaving the registry, so nothing scans its stack once
   the joiner may free it. */
__attribute__((noreturn)) static void kt_thread_main(void *start) {
    KThread *thread = (KThread *)start;
    uintptr_t bottom = 0;
    kt_lock();
    kt_running = thread;
    thread->tid = kt_sys_gettid();
    thread->stack_bottom = (uintptr_t)&bottom;
    thread->released = false;
    kt_pending = NULL;
    void (*routine)(KRef) = thread->start_routine;
    KRef argument = thread->start_argument;
    thread->start_argument = NULL;
    routine(argument);
    kt_end_on_uncaught(thread);
    kt_thread_remove(thread);
    kt_running = NULL;
    kt_unlock();
    kt_sys_exit_thread();
}

static long kt_spawn(KThreadHandle *handle, KThread *thread) {
    handle->stack = kt_map(KT_THREAD_STACK_BYTES);
    /* A guard below the stack: a thread that recurses past its stack faults on it, deterministically,
       instead of running on into whatever the kernel mapped next to it. */
    kt_protect_none(handle->stack, KT_GUARD_BYTES);
    __atomic_store_n(&handle->exited, 1, __ATOMIC_RELEASE);
    return kt_clone(KT_CLONE_FLAGS, (char *)handle->stack + KT_THREAD_STACK_BYTES, &handle->exited,
                    kt_thread_main, thread);
}

KThreadHandle *kt_thread_start(void (*routine)(KRef), KRef argument) {
    /* The new thread is registered before it exists, released and with nothing on its stack yet,
       so a collection before it first runs keeps its argument; it waits for the lock this thread
       holds. */
    KThread *thread = kt_thread_new(0, 0);
    thread->released = true;
    thread->start_routine = routine;
    thread->start_argument = argument;
    thread->number = kt_numbered_threads++;
    KThreadHandle *handle = (KThreadHandle *)kt_map(sizeof(KThreadHandle));
    if (kt_spawn(handle, thread) <= 0) {
        KT_FAIL("krusty: could not start a thread\n");
    }
    return handle;
}

void kt_thread_join(KThreadHandle *handle) {
    KThread *self = kt_native_enter();
    while (__atomic_load_n(&handle->exited, __ATOMIC_ACQUIRE) != 0) {
        /* The kernel's wake when a thread ends is a shared futex wake. */
        kt_sys_futex_wait_shared(&handle->exited, 1);
    }
    kt_native_leave(self);
    kt_unmap(handle->stack, KT_THREAD_STACK_BYTES);
    kt_unmap(handle, sizeof(KThreadHandle));
}

/* ---- roots -------------------------------------------------------------------------------------- */

uintptr_t kt_threads_running_bottom(void) {
    return kt_running == NULL ? 0 : kt_running->stack_bottom;
}

void kt_threads_scan_released(void) {
    for (KThread *thread = kt_threads; thread != NULL; thread = thread->next) {
        if (thread == kt_running) {
            continue;
        }
        if (!thread->released) {
            KT_FAIL("krusty: a collection while another thread runs Kotlin\n");
        }
        for (unsigned i = 0; i < KT_SAVED_REGISTERS; i++) {
            kt_gc_scan_word(thread->registers[i]);
        }
        kt_gc_scan_word((uintptr_t)thread->pending);
        kt_gc_scan_word((uintptr_t)thread->start_argument);
        kt_gc_scan_range(thread->saved_sp, thread->stack_bottom);
    }
}
