/* krusty native runtime: the POSIX layer a static program links instead of a C library.

   A program that links no C library still calls POSIX: `platform.posix` is what Kotlin/Native code
   reaches the system through. Here those functions are served the way Go serves its `syscall`
   package, straight from the kernel, so a static program needs no libc for any target and krusty
   cross-compiles it from any host. Each function has the C library's name, signature, errno
   behavior and struct layout for the target, so a binding written against the target's glibc
   headers calls it unchanged; a program that links a real C library links the library's instead,
   and none of these translation units.

   This header is the layer's own, shared by its translation units and never by the rest of the
   runtime, which keeps to its `kt_` wrappers. Hand-written freestanding C; it includes no system
   header, so every number and layout below is the target's, spelled out. */
#ifndef KRUSTY_POSIX_H
#define KRUSTY_POSIX_H

#include "krusty_internal.h"

#include <stdarg.h>

/* ---- system calls the layer makes beyond the runtime's own ------------------------------------ */

#if defined(__x86_64__)
#define KT_SYS_CLOSE 3
#define KT_SYS_LSEEK 8
#define KT_SYS_RT_SIGACTION 13
#define KT_SYS_RT_SIGPROCMASK 14
#define KT_SYS_RT_SIGRETURN 15
#define KT_SYS_SCHED_YIELD 24
#define KT_SYS_NANOSLEEP 35
#define KT_SYS_GETPID 39
#define KT_SYS_SOCKET 41
#define KT_SYS_CONNECT 42
#define KT_SYS_SENDTO 44
#define KT_SYS_RECVFROM 45
#define KT_SYS_SHUTDOWN 48
#define KT_SYS_BIND 49
#define KT_SYS_LISTEN 50
#define KT_SYS_GETSOCKNAME 51
#define KT_SYS_GETPEERNAME 52
#define KT_SYS_SETSOCKOPT 54
#define KT_SYS_GETSOCKOPT 55
#define KT_SYS_KILL 62
#define KT_SYS_TGKILL 234
#define KT_SYS_FCNTL 72
#define KT_SYS_ARCH_PRCTL 158
#define KT_SYS_SCHED_GETAFFINITY 204
#define KT_SYS_GETDENTS64 217
#define KT_SYS_CLOCK_GETTIME 228
#define KT_SYS_EPOLL_CTL 233
#define KT_SYS_OPENAT 257
#define KT_SYS_MKDIRAT 258
#define KT_SYS_NEWFSTATAT 262
#define KT_SYS_UNLINKAT 263
#define KT_SYS_EPOLL_PWAIT 281
#define KT_SYS_ACCEPT4 288
#define KT_SYS_EPOLL_CREATE1 291
#define KT_SYS_PIPE2 293
#else /* aarch64 and riscv64 share the generic table */
#define KT_SYS_EPOLL_CREATE1 20
#define KT_SYS_EPOLL_CTL 21
#define KT_SYS_EPOLL_PWAIT 22
#define KT_SYS_FCNTL 25
#define KT_SYS_MKDIRAT 34
#define KT_SYS_UNLINKAT 35
#define KT_SYS_OPENAT 56
#define KT_SYS_CLOSE 57
#define KT_SYS_PIPE2 59
#define KT_SYS_GETDENTS64 61
#define KT_SYS_LSEEK 62
#define KT_SYS_NEWFSTATAT 79
#define KT_SYS_NANOSLEEP 101
#define KT_SYS_CLOCK_GETTIME 113
#define KT_SYS_SCHED_GETAFFINITY 123
#define KT_SYS_SCHED_YIELD 124
#define KT_SYS_KILL 129
#define KT_SYS_TGKILL 131
#define KT_SYS_RT_SIGACTION 134
#define KT_SYS_RT_SIGPROCMASK 135
#define KT_SYS_RT_SIGRETURN 139
#define KT_SYS_GETPID 172
#define KT_SYS_SOCKET 198
#define KT_SYS_BIND 200
#define KT_SYS_LISTEN 201
#define KT_SYS_CONNECT 203
#define KT_SYS_GETSOCKNAME 204
#define KT_SYS_GETPEERNAME 205
#define KT_SYS_SENDTO 206
#define KT_SYS_RECVFROM 207
#define KT_SYS_SETSOCKOPT 208
#define KT_SYS_GETSOCKOPT 209
#define KT_SYS_SHUTDOWN 210
#define KT_SYS_ACCEPT4 242
#endif

/* ---- each thread's own state: errno and pthread_self -------------------------------------------

   A C program expects `errno` per thread, which a C library keeps in thread-local storage behind
   the thread pointer. With no C library the layer owns the thread pointer: every thread the
   program has points it at its own block, whose first word points back at the block, as the
   x86_64 TLS ABI requires of whatever `%fs` points at. The block is also the thread's
   `pthread_t`. */
typedef struct KOsThread KOsThread;
struct KOsThread {
    KOsThread *self;
    int error;
    /* Cleared by the kernel, and woken, when the thread ends: what `pthread_join` waits on. */
    uint32_t running;
    /* Who frees the thread's mapping: KT_OS_THREAD_JOINABLE until the thread is detached or its
       routine returns, whichever comes first, which decides it. */
    uint32_t ownership;
    /* The mapping this block sits at the top of, the thread's stack below it; NULL for a block
       the thread did not get from `pthread_create`. */
    void *mapping;
    size_t mapping_bytes;
    void *(*routine)(void *);
    void *argument;
    void *result;
    KOsThread *next_finished;
};

#define KT_OS_THREAD_JOINABLE 0
#define KT_OS_THREAD_DETACHED 1
#define KT_OS_THREAD_FINISHED 2

/* The calling thread's block; NULL on a thread nothing gave one, which only a thread foreign code
   started without the layer can be. */
KOsThread *kt_os_thread_self(void);

/* Point the calling thread's thread pointer at `block`. */
void kt_os_thread_set(KOsThread *block);

int *__errno_location(void);

/* A raw system call's answer as a C library function's: a negated errno in -4095..-1 becomes -1
   with `errno` set; anything else is the answer itself. */
static inline long kt_posix_result(long answer) {
    if ((unsigned long)answer > (unsigned long)-4096L) {
        *__errno_location() = (int)-answer;
        return -1;
    }
    return answer;
}

/* ---- numbers the layer interprets itself ------------------------------------------------------- */

#define KT_AT_FDCWD (-100)
#define KT_AT_SYMLINK_NOFOLLOW 0x100
#define KT_AT_REMOVEDIR 0x200
#define KT_AT_EMPTY_PATH 0x1000

#define KT_O_RDONLY 0
#define KT_O_WRONLY 1
#define KT_O_RDWR 2
#define KT_O_CREAT 0100
#define KT_O_TRUNC 01000
#define KT_O_APPEND 02000
#define KT_O_CLOEXEC 02000000
/* arm64 has its own <asm/fcntl.h>, where O_DIRECTORY and O_DIRECT trade places with the
   asm-generic values every other supported target uses: 040000 there is O_DIRECTORY, and
   0200000 is O_DIRECT. */
#if defined(__aarch64__)
#define KT_O_DIRECTORY 040000
#else
#define KT_O_DIRECTORY 0200000
#endif

/* fcntl's commands, the same numbers on every supported target. */
#define KT_F_DUPFD 0
#define KT_F_GETFD 1
#define KT_F_SETFD 2
#define KT_F_GETFL 3
#define KT_F_SETFL 4
#define KT_F_GETLK 5
#define KT_F_SETLK 6
#define KT_F_SETLKW 7
#define KT_F_SETOWN 8
#define KT_F_GETOWN 9
#define KT_F_DUPFD_CLOEXEC 1030

/* `sysconf`'s names, glibc's own numbering rather than the kernel's. */
#define KT_SC_CLK_TCK 2
#define KT_SC_OPEN_MAX 4
#define KT_SC_PAGESIZE 30
#define KT_SC_NPROCESSORS_CONF 83
#define KT_SC_NPROCESSORS_ONLN 84

#define KT_SA_RESTORER 0x04000000ul
#define KT_SA_RESTART 0x10000000

#define KT_EBADF 9
#define KT_ENOMEM 12
#define KT_EINVAL 22
#define KT_EBUSY 16
#define KT_ETIMEDOUT 110

#endif /* KRUSTY_POSIX_H */
