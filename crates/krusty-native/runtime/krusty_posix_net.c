/* krusty native runtime, POSIX layer: sockets and epoll. Each function is its system call: the
   address structures, socket options and `struct epoll_event` (packed on x86_64, naturally aligned
   elsewhere, the same in glibc and the kernel) pass through untouched. Hand-written freestanding C;
   see krusty_posix.h. */
#include "krusty_posix.h"

int socket(int domain, int type, int protocol) {
    return (int)kt_posix_result(kt_syscall(KT_SYS_SOCKET, domain, type, protocol, 0, 0, 0));
}

int bind(int fd, const void *address, unsigned length) {
    return (int)kt_posix_result(kt_syscall(KT_SYS_BIND, fd, (long)address, length, 0, 0, 0));
}

int listen(int fd, int backlog) {
    return (int)kt_posix_result(kt_syscall(KT_SYS_LISTEN, fd, backlog, 0, 0, 0, 0));
}

int accept4(int fd, void *address, unsigned *length, int flags) {
    return (int)kt_posix_result(
        kt_syscall(KT_SYS_ACCEPT4, fd, (long)address, (long)length, flags, 0, 0));
}

int accept(int fd, void *address, unsigned *length) { return accept4(fd, address, length, 0); }

int connect(int fd, const void *address, unsigned length) {
    return (int)kt_posix_result(kt_syscall(KT_SYS_CONNECT, fd, (long)address, length, 0, 0, 0));
}

int getsockname(int fd, void *address, unsigned *length) {
    return (int)kt_posix_result(
        kt_syscall(KT_SYS_GETSOCKNAME, fd, (long)address, (long)length, 0, 0, 0));
}

int getpeername(int fd, void *address, unsigned *length) {
    return (int)kt_posix_result(
        kt_syscall(KT_SYS_GETPEERNAME, fd, (long)address, (long)length, 0, 0, 0));
}

int setsockopt(int fd, int level, int name, const void *value, unsigned length) {
    return (int)kt_posix_result(
        kt_syscall(KT_SYS_SETSOCKOPT, fd, level, name, (long)value, length, 0));
}

int getsockopt(int fd, int level, int name, void *value, unsigned *length) {
    return (int)kt_posix_result(
        kt_syscall(KT_SYS_GETSOCKOPT, fd, level, name, (long)value, (long)length, 0));
}

long sendto(int fd, const void *bytes, size_t length, int flags, const void *address,
            unsigned address_length) {
    return kt_posix_result(kt_syscall(KT_SYS_SENDTO, fd, (long)bytes, (long)length, flags,
                                      (long)address, address_length));
}

long send(int fd, const void *bytes, size_t length, int flags) {
    return sendto(fd, bytes, length, flags, NULL, 0);
}

long recvfrom(int fd, void *bytes, size_t length, int flags, void *address,
              unsigned *address_length) {
    return kt_posix_result(kt_syscall(KT_SYS_RECVFROM, fd, (long)bytes, (long)length, flags,
                                      (long)address, (long)address_length));
}

long recv(int fd, void *bytes, size_t length, int flags) {
    return recvfrom(fd, bytes, length, flags, NULL, NULL);
}

int shutdown(int fd, int how) {
    return (int)kt_posix_result(kt_syscall(KT_SYS_SHUTDOWN, fd, how, 0, 0, 0, 0));
}

int epoll_create1(int flags) {
    return (int)kt_posix_result(kt_syscall(KT_SYS_EPOLL_CREATE1, flags, 0, 0, 0, 0, 0));
}

int epoll_create(int size) {
    if (size <= 0) {
        *__errno_location() = KT_EINVAL;
        return -1;
    }
    return epoll_create1(0);
}

int epoll_ctl(int fd, int operation, int target, void *event) {
    return (int)kt_posix_result(
        kt_syscall(KT_SYS_EPOLL_CTL, fd, operation, target, (long)event, 0, 0));
}

/* `epoll_pwait` with no mask is `epoll_wait`, and the generic table has only the former. The last
   argument is the size of the kernel's signal set, ignored with no set. */
int epoll_wait(int fd, void *events, int capacity, int timeout) {
    return (int)kt_posix_result(
        kt_syscall(KT_SYS_EPOLL_PWAIT, fd, (long)events, capacity, timeout, 0, 8));
}
