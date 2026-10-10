/* What a POSIX-layer driver shares. It is compiled against the HOST C library's own headers, not
   the runtime's, and linked with no C library at all: every POSIX function it calls is the
   runtime's, and every struct it passes has glibc's layout, which is the point. A layer whose
   layout or errno differed from glibc's would answer the driver wrong here, where a test written
   against the layer's own declarations would agree with it. */
#ifndef KRUSTY_DRIVER_POSIX_H
#define KRUSTY_DRIVER_POSIX_H

#define _GNU_SOURCE
#include <errno.h>
#include <string.h>
#include <unistd.h>

void kt_program_entry(void);

static inline void driver_say(const char *text) { (void)!write(1, text, strlen(text)); }

/* Report what failed, on stderr, and end the way a failed driver does. */
#define DRIVER_CHECK(condition, what)                                                              \
    do {                                                                                           \
        if (!(condition)) {                                                                        \
            (void)!write(2, what "\n", sizeof(what));                                              \
            _exit(134);                                                                            \
        }                                                                                          \
    } while (0)

#endif /* KRUSTY_DRIVER_POSIX_H */
