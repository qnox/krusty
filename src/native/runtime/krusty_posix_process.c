/* krusty native runtime, POSIX layer: the process. The environment and auxiliary vector, `sysconf`,
   signals, clocks and sleeping, and ending the process. Hand-written freestanding C; see
   krusty_posix.h. */
#include "krusty_posix.h"

/* ---- the environment --------------------------------------------------------------------------- */

char *getenv(const char *name) {
    char **environment = kt_process_environment();
    if (environment == NULL) {
        return NULL;
    }
    for (char **entry = environment; *entry != NULL; entry++) {
        const char *at = *entry;
        const char *wanted = name;
        while (*wanted != '\0' && *at == *wanted) {
            at++;
            wanted++;
        }
        if (*wanted == '\0' && *at == '=') {
            return (char *)(at + 1);
        }
    }
    return NULL;
}

/* The value the kernel's auxiliary vector, which follows the environment's NULL on the initial
   stack, holds for `key`, or `otherwise`. */
static unsigned long kt_auxiliary_value(unsigned long key, unsigned long otherwise) {
    char **entry = kt_process_environment();
    if (entry == NULL) {
        return otherwise;
    }
    while (*entry != NULL) {
        entry++;
    }
    for (unsigned long *pair = (unsigned long *)(entry + 1); pair[0] != 0; pair += 2) {
        if (pair[0] == key) {
            return pair[1];
        }
    }
    return otherwise;
}

/* ---- sysconf ----------------------------------------------------------------------------------- */

#define KT_AT_PAGESZ 6

/* The processors this process may run on: the bits of its affinity mask, as glibc counts them. */
static long kt_processors(void) {
    unsigned long mask[16] = {0};
    long filled = kt_syscall(KT_SYS_SCHED_GETAFFINITY, 0, sizeof mask, (long)mask, 0, 0, 0);
    if (filled <= 0) {
        return 1;
    }
    long count = 0;
    for (long word = 0; word < filled / (long)sizeof(unsigned long); word++) {
        count += __builtin_popcountl(mask[word]);
    }
    return count > 0 ? count : 1;
}

int getpagesize(void) { return (int)kt_auxiliary_value(KT_AT_PAGESZ, 4096); }

long sysconf(int name) {
    switch (name) {
    case KT_SC_CLK_TCK:
        return 100;
    case KT_SC_OPEN_MAX:
        return 1024;
    case KT_SC_PAGESIZE:
        return getpagesize();
    case KT_SC_NPROCESSORS_CONF:
    case KT_SC_NPROCESSORS_ONLN:
        return kt_processors();
    default:
        *__errno_location() = KT_EINVAL;
        return -1;
    }
}

/* ---- signals ------------------------------------------------------------------------------------

   glibc's `struct sigaction` is the handler, a 1024-bit `sigset_t`, the flags as an int and a
   restorer, on every supported target; the kernel's is the handler, the flags as a long, the
   restorer where the architecture has one, and a 64-bit mask. Only the first word of glibc's set
   means anything to Linux, and on these little-endian targets it is the kernel's mask, so
   `sigprocmask` passes sets through. */

typedef struct {
    void *handler;
    unsigned long mask[16];
    int flags;
    void (*restorer)(void);
} KSignalAction;

typedef struct {
    void *handler;
    unsigned long flags;
#if !defined(__riscv)
    void (*restorer)(void);
#endif
    unsigned long mask;
} KKernelSignalAction;

/* Where a handler returns to: back into the kernel, which restores what the signal interrupted.
   x86_64 has no other way back. aarch64 would fall back on the vDSO's, but naming one costs
   nothing. riscv64 has only the vDSO's. */
#if defined(__x86_64__)
void kt_signal_return(void);
__asm__(".text\n"
        ".globl kt_signal_return\n"
        ".type kt_signal_return, @function\n"
        "kt_signal_return:\n"
        "  movl $15, %eax\n"
        "  syscall\n"
        "  ud2\n"
        ".size kt_signal_return, .-kt_signal_return\n");
#elif defined(__aarch64__)
void kt_signal_return(void);
__asm__(".text\n"
        ".globl kt_signal_return\n"
        ".type kt_signal_return, %function\n"
        "kt_signal_return:\n"
        "  mov x8, #139\n"
        "  svc #0\n"
        "  brk #0\n"
        ".size kt_signal_return, .-kt_signal_return\n");
#endif

int sigaction(int signal, const KSignalAction *action, KSignalAction *previous) {
    KKernelSignalAction given = {0};
    KKernelSignalAction old = {0};
    if (action != NULL) {
        given.handler = action->handler;
        given.flags = (unsigned long)(unsigned)action->flags;
        given.mask = action->mask[0];
#if !defined(__riscv)
        given.flags |= KT_SA_RESTORER;
        given.restorer = kt_signal_return;
#endif
    }
    long answer = kt_syscall(KT_SYS_RT_SIGACTION, signal, action != NULL ? (long)&given : 0,
                             previous != NULL ? (long)&old : 0, 8, 0, 0);
    if (answer < 0) {
        return (int)kt_posix_result(answer);
    }
    if (previous != NULL) {
        memset(previous, 0, sizeof(KSignalAction));
        previous->handler = old.handler;
        previous->flags = (int)old.flags;
        previous->mask[0] = old.mask;
#if !defined(__riscv)
        previous->restorer = old.restorer;
#endif
    }
    return 0;
}

/* `signal` as glibc defines it: BSD semantics, the handler staying installed and an interrupted
   system call restarted. SIG_ERR, -1, on failure. */
void *signal(int number, void *handler) {
    KSignalAction action = {0};
    KSignalAction previous;
    action.handler = handler;
    action.flags = KT_SA_RESTART;
    if (sigaction(number, &action, &previous) < 0) {
        return (void *)-1;
    }
    return previous.handler;
}

int sigemptyset(unsigned long *set) {
    memset(set, 0, 16 * sizeof(unsigned long));
    return 0;
}

int sigfillset(unsigned long *set) {
    memset(set, 0xff, 16 * sizeof(unsigned long));
    return 0;
}

static bool kt_signal_number(int number) {
    if (number <= 0 || number > 64) {
        *__errno_location() = KT_EINVAL;
        return false;
    }
    return true;
}

int sigaddset(unsigned long *set, int number) {
    if (!kt_signal_number(number)) {
        return -1;
    }
    set[(number - 1) / 64] |= 1ul << ((number - 1) % 64);
    return 0;
}

int sigdelset(unsigned long *set, int number) {
    if (!kt_signal_number(number)) {
        return -1;
    }
    set[(number - 1) / 64] &= ~(1ul << ((number - 1) % 64));
    return 0;
}

int sigismember(const unsigned long *set, int number) {
    if (!kt_signal_number(number)) {
        return -1;
    }
    return (set[(number - 1) / 64] >> ((number - 1) % 64)) & 1;
}

int sigprocmask(int how, const unsigned long *set, unsigned long *previous) {
    return (int)kt_posix_result(
        kt_syscall(KT_SYS_RT_SIGPROCMASK, how, (long)set, (long)previous, 8, 0, 0));
}

/* The same call, which on Linux masks the calling thread's signals alone; an error number rather
   than errno. */
int pthread_sigmask(int how, const unsigned long *set, unsigned long *previous) {
    long answer = kt_syscall(KT_SYS_RT_SIGPROCMASK, how, (long)set, (long)previous, 8, 0, 0);
    return answer < 0 ? (int)-answer : 0;
}

int getpid(void) { return (int)kt_syscall(KT_SYS_GETPID, 0, 0, 0, 0, 0, 0); }

int kill(int process, int number) {
    return (int)kt_posix_result(kt_syscall(KT_SYS_KILL, process, number, 0, 0, 0, 0));
}

/* The calling thread, not the process: a process-directed signal may run its handler on any thread
   that does not block it, where `raise` promises the caller's. */
int raise(int number) {
    return (int)kt_posix_result(
        kt_syscall(KT_SYS_TGKILL, getpid(), kt_sys_gettid(), number, 0, 0, 0));
}

/* ---- clocks and sleeping ------------------------------------------------------------------------

   `struct timespec` is two longs, seconds and nanoseconds, in glibc and the kernel alike. */

int clock_gettime(int clock, void *time) {
    return (int)kt_posix_result(kt_syscall(KT_SYS_CLOCK_GETTIME, clock, (long)time, 0, 0, 0, 0));
}

long time(long *into) {
    long now[2];
    kt_syscall(KT_SYS_CLOCK_GETTIME, 0 /* CLOCK_REALTIME */, (long)now, 0, 0, 0, 0);
    if (into != NULL) {
        *into = now[0];
    }
    return now[0];
}

/* `struct timeval`: seconds and microseconds. The time zone argument is obsolete; glibc ignores
   it too. */
int gettimeofday(long *time, void *zone) {
    (void)zone;
    long now[2];
    long answer = kt_syscall(KT_SYS_CLOCK_GETTIME, 0, (long)now, 0, 0, 0, 0);
    if (answer < 0) {
        return (int)kt_posix_result(answer);
    }
    time[0] = now[0];
    time[1] = now[1] / 1000;
    return 0;
}

int nanosleep(const long *duration, long *remaining) {
    return (int)kt_posix_result(
        kt_syscall(KT_SYS_NANOSLEEP, (long)duration, (long)remaining, 0, 0, 0, 0));
}

int usleep(unsigned microseconds) {
    long duration[2] = {microseconds / 1000000, (long)(microseconds % 1000000) * 1000};
    return nanosleep(duration, NULL);
}

unsigned sleep(unsigned seconds) {
    long duration[2] = {seconds, 0};
    long remaining[2] = {0, 0};
    if (nanosleep(duration, remaining) < 0) {
        return (unsigned)remaining[0] + (remaining[1] > 0);
    }
    return 0;
}

/* ---- ending the process -------------------------------------------------------------------------

   Writes are unbuffered, so `exit` has nothing to flush and no `atexit` handlers to run. */

__attribute__((noreturn)) void _exit(int status) { kt_sys_exit(status); }

__attribute__((noreturn)) void exit(int status) { kt_sys_exit(status); }

/* SIGABRT, unblocked, so that a handler the program installed runs first; if that handler returns,
   SIGABRT again with its default action, which ends the process on the signal. A blocked SIGABRT
   would only be left pending, and the process would end with an ordinary exit status. */
__attribute__((noreturn)) void abort(void) {
    unsigned long abort_only = 1UL << (6 - 1);
    kt_syscall(KT_SYS_RT_SIGPROCMASK, 1 /* SIG_UNBLOCK */, (long)&abort_only, 0, 8, 0, 0);
    raise(6);
    KSignalAction action = {0};
    sigaction(6, &action, NULL);
    raise(6);
    kt_sys_exit(134);
}
