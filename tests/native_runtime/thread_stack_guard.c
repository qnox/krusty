/* A thread the runtime starts runs on a stack with an inaccessible guard directly below it, so a
   thread that recurses past its stack faults there instead of running on into whatever the kernel
   mapped next. The thread finds its own stack in /proc/self/maps, by the address of one of its
   locals, and checks the mapping just below it. */
#include "krusty_rt.h"
#include "krusty_sys.h"

#if defined(__x86_64__)
#define DRIVER_SYS_READ 0
#define DRIVER_SYS_OPENAT 257
#define DRIVER_SYS_CLOSE 3
#else
#define DRIVER_SYS_READ 63
#define DRIVER_SYS_OPENAT 56
#define DRIVER_SYS_CLOSE 57
#endif

static char maps[1 << 16];
static kt_boolean checked;

static uintptr_t hex(const char **at) {
    uintptr_t value = 0;
    for (;;) {
        char digit = **at;
        if (digit >= '0' && digit <= '9') {
            value = value * 16 + (uintptr_t)(digit - '0');
        } else if (digit >= 'a' && digit <= 'f') {
            value = value * 16 + (uintptr_t)(digit - 'a' + 10);
        } else {
            return value;
        }
        (*at)++;
    }
}

static void routine(KRef argument) {
    (void)argument;
    volatile char local = 0;
    uintptr_t here = (uintptr_t)&local;
    long fd = kt_syscall(DRIVER_SYS_OPENAT, -100, (long)"/proc/self/maps", 0, 0, 0, 0);
    if (fd < 0) {
        KT_SYS_FAIL("/proc/self/maps did not open\n");
    }
    size_t filled = 0;
    long step;
    while ((step = kt_syscall(DRIVER_SYS_READ, fd, (long)(maps + filled),
                               (long)(sizeof maps - 1 - filled), 0, 0, 0)) > 0) {
        filled += (size_t)step;
    }
    kt_syscall(DRIVER_SYS_CLOSE, fd, 0, 0, 0, 0, 0);
    maps[filled] = '\0';

    /* Each line: start-end perms ... */
    uintptr_t previous_start = 0, previous_end = 0;
    char previous_perms[4] = {0};
    for (const char *line = maps; *line != '\0';) {
        const char *at = line;
        uintptr_t start = hex(&at);
        at++;
        uintptr_t end = hex(&at);
        at++;
        if (start <= here && here < end) {
            if (at[0] != 'r' || at[1] != 'w') {
                KT_SYS_FAIL("the thread's stack is not readable and writable\n");
            }
            if (previous_end != start || previous_end - previous_start < 4096 ||
                previous_perms[0] != '-' || previous_perms[1] != '-' || previous_perms[2] != '-') {
                KT_SYS_FAIL("the thread's stack has no inaccessible guard directly below it\n");
            }
            checked = true;
            return;
        }
        previous_start = start;
        previous_end = end;
        previous_perms[0] = at[0];
        previous_perms[1] = at[1];
        previous_perms[2] = at[2];
        while (*line != '\0' && *line != '\n') {
            line++;
        }
        if (*line == '\n') {
            line++;
        }
    }
    KT_SYS_FAIL("the thread's stack is not in /proc/self/maps\n");
}

void kt_program_entry(void) {
    uintptr_t bottom = 0;
    kt_runtime_init(&bottom);
    kt_thread_join(kt_thread_start(routine, NULL));
    if (!checked) {
        KT_SYS_FAIL("the started thread did not run\n");
    }
    kt_sys_write(1, "OK\n", 3);
}
