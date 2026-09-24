/* krusty native runtime: the system-call shim. Hand-written freestanding C; build.rs compiles it
   into the runtime for every target. */
#ifndef KRUSTY_SYS_H
#define KRUSTY_SYS_H

#include <stddef.h>
#include <stdint.h>

#if defined(__x86_64__)
#define KT_SYS_WRITE 1
#define KT_SYS_MMAP 9
#define KT_SYS_MUNMAP 11
#define KT_SYS_EXIT 231 /* exit_group */
#elif defined(__aarch64__) || (defined(__riscv) && __riscv_xlen == 64)
#define KT_SYS_WRITE 64
#define KT_SYS_MMAP 222
#define KT_SYS_MUNMAP 215
#define KT_SYS_EXIT 94 /* exit_group */
#else
#error "krusty native: unsupported architecture"
#endif

static inline long kt_syscall(long number, long a0, long a1, long a2, long a3, long a4, long a5) {
#if defined(__x86_64__)
    /* The syscall ABI passes the fourth argument in r10, not rcx: `syscall` clobbers rcx. */
    register long r10 __asm__("r10") = a3;
    register long r8 __asm__("r8") = a4;
    register long r9 __asm__("r9") = a5;
    long result;
    __asm__ volatile("syscall"
                     : "=a"(result)
                     : "a"(number), "D"(a0), "S"(a1), "d"(a2), "r"(r10), "r"(r8), "r"(r9)
                     : "rcx", "r11", "memory");
    return result;
#elif defined(__aarch64__)
    register long x8 __asm__("x8") = number;
    register long x0 __asm__("x0") = a0;
    register long x1 __asm__("x1") = a1;
    register long x2 __asm__("x2") = a2;
    register long x3 __asm__("x3") = a3;
    register long x4 __asm__("x4") = a4;
    register long x5 __asm__("x5") = a5;
    __asm__ volatile("svc #0"
                     : "+r"(x0)
                     : "r"(x8), "r"(x1), "r"(x2), "r"(x3), "r"(x4), "r"(x5)
                     : "memory");
    return x0;
#else /* riscv64 */
    register long a7r __asm__("a7") = number;
    register long a0r __asm__("a0") = a0;
    register long a1r __asm__("a1") = a1;
    register long a2r __asm__("a2") = a2;
    register long a3r __asm__("a3") = a3;
    register long a4r __asm__("a4") = a4;
    register long a5r __asm__("a5") = a5;
    __asm__ volatile("ecall"
                     : "+r"(a0r)
                     : "r"(a7r), "r"(a1r), "r"(a2r), "r"(a3r), "r"(a4r), "r"(a5r)
                     : "memory");
    return a0r;
#endif
}

__attribute__((noreturn)) static inline void kt_sys_exit(long status) {
    kt_syscall(KT_SYS_EXIT, status, 0, 0, 0, 0, 0);
    __builtin_unreachable();
}

#define KT_EINTR 4
#define KT_EAGAIN 11

static inline void kt_sys_write(long fd, const char *bytes, size_t length) {
    size_t written = 0;
    while (written < length) {
        long step = kt_syscall(KT_SYS_WRITE, fd, (long)(bytes + written), (long)(length - written),
                               0, 0, 0);
        /* A short write is normal, and so are an interrupted one and a full non-blocking pipe: the
           rest of the buffer can still be written, so dropping it would lose output that the
           reader is about to accept. Every other error (a closed pipe, a bad descriptor) ends the
           write the way the JVM's `PrintStream` ends it: silently, because printing has no caller
           to report to. */
        if (step == -KT_EINTR || step == -KT_EAGAIN) {
            continue;
        }
        if (step <= 0) {
            return;
        }
        written += (size_t)step;
    }
}

/* Print `message` on stderr and exit the way a SIGABRT looks to a shell. */
static inline void kt_sys_fail(const char *message, size_t length) {
    kt_sys_write(2, message, length);
    kt_sys_exit(134);
}

#define KT_SYS_FAIL(literal) kt_sys_fail(literal, sizeof(literal) - 1)

static inline void kt_fail_oom(void) { KT_SYS_FAIL("krusty: out of memory\n"); }

/* Map `bytes` of fresh, zero-filled, readable and writable memory. Anonymous mappings are
   zero-filled by the kernel; callers rely on that instead of clearing. Exits on failure: there is
   no caller that could do anything else with a failed mapping. */
static inline void *kt_map(size_t bytes) {
    /* PROT_READ|PROT_WRITE, MAP_PRIVATE|MAP_ANONYMOUS, no file. */
    long mapped = kt_syscall(KT_SYS_MMAP, 0, (long)bytes, 3, 0x22, -1, 0);
    if (mapped <= 0 && mapped >= -4095) {
        kt_fail_oom();
    }
    return (void *)mapped;
}

static inline void kt_unmap(void *address, size_t bytes) {
    kt_syscall(KT_SYS_MUNMAP, (long)address, (long)bytes, 0, 0, 0, 0);
}

#endif /* KRUSTY_SYS_H */
