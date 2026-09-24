/* A write to a full NON-BLOCKING pipe answers `-EAGAIN`, and one a signal interrupts answers
   `-EINTR`. Neither means the rest cannot be written: `kt_sys_write` must keep going, where it used
   to stop and silently drop the tail of the buffer.

   The driver turns its own stdout non-blocking and writes far more than a pipe holds, so the
   harness only reads the whole payload if every `-EAGAIN` was retried. */
#include "krusty_sys.h"

#if defined(__x86_64__)
#define KT_SYS_FCNTL 72
#else
#define KT_SYS_FCNTL 25
#endif
#define KT_F_GETFL 3
#define KT_F_SETFL 4
#define KT_O_NONBLOCK 04000

#define PAYLOAD (1u << 20)

static char payload[PAYLOAD];

void kt_program_entry(void) {
    long flags = kt_syscall(KT_SYS_FCNTL, 1, KT_F_GETFL, 0, 0, 0, 0);
    if (flags < 0 || kt_syscall(KT_SYS_FCNTL, 1, KT_F_SETFL, flags | KT_O_NONBLOCK, 0, 0, 0) < 0) {
        KT_SYS_FAIL("cannot make stdout non-blocking\n");
    }
    for (unsigned i = 0; i < PAYLOAD; i++) {
        payload[i] = (char)('a' + i % 26);
    }
    kt_sys_write(1, payload, PAYLOAD);
    kt_sys_write(1, "OK\n", 3);
}
