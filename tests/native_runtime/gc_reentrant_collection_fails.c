/* A collection that starts while one is running is a bug, and it must end the program loudly. It
   used to return at once, so the allocation that asked for it went ahead on a heap that had not
   been swept, beside a collection whose marks were half set.

   Nothing inside the collector allocates, so the driver re-enters it the way a program could: from
   a signal handler. A registered global slot points into a page nothing may read, so the collector
   faults while reading its roots, and the SIGSEGV handler asks for another collection. The harness
   expects the collector's own failure; anything this driver prints itself is the defect. */
#include "krusty_rt.h"
#include "krusty_sys.h"

#if defined(__x86_64__)
#define KT_SYS_RT_SIGACTION 13
#else
#define KT_SYS_RT_SIGACTION 134
#endif
#define KT_SIGSEGV 11
#define KT_PROT_NONE 0
#define KT_MAP_PRIVATE_ANONYMOUS 0x22

/* The kernel's own `struct sigaction`, which is not libc's. x86_64 and aarch64 carry a
   restorer, and x86_64 refuses to deliver a signal without one. The handler never returns, so the
   restorer is never called. */
#if defined(__x86_64__) || defined(__aarch64__)
#define KT_SA_RESTORER 0x04000000ul
struct kt_sigaction {
    void (*handler)(int);
    unsigned long flags;
    void (*restorer)(void);
    unsigned long mask;
};
#else
#define KT_SA_RESTORER 0ul
struct kt_sigaction {
    void (*handler)(int);
    unsigned long flags;
    unsigned long mask;
};
#endif

static void on_fault(int signal) {
    (void)signal;
    kt_gc_collect();
    KT_SYS_FAIL("a collection started during a collection returned\n");
}

static void never_restored(void) { KT_SYS_FAIL("the fault handler returned\n"); }

void kt_program_entry(void) {
    uintptr_t bottom = 0;
    kt_runtime_init(&bottom);

    struct kt_sigaction action = {.handler = on_fault, .flags = KT_SA_RESTORER};
#if defined(__x86_64__) || defined(__aarch64__)
    action.restorer = never_restored;
#else
    (void)never_restored;
#endif
    if (kt_syscall(KT_SYS_RT_SIGACTION, KT_SIGSEGV, (long)&action, 0, sizeof action.mask, 0, 0) !=
        0) {
        KT_SYS_FAIL("cannot install the fault handler\n");
    }
    long unreadable =
        kt_syscall(KT_SYS_MMAP, 0, 4096, KT_PROT_NONE, KT_MAP_PRIVATE_ANONYMOUS, -1, 0);
    if (unreadable <= 0 && unreadable >= -4095) {
        KT_SYS_FAIL("cannot map an unreadable page\n");
    }
    kt_gc_add_global_root((void **)unreadable);
    kt_gc_collect();
    KT_SYS_FAIL("reading an unreadable root did not fault\n");
}
