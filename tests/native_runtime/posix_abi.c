/* Every number the POSIX layer copied by hand, checked at compile time against the target's own
   C library and kernel headers: a value copied from another architecture, or from the wrong
   header, fails the build for that target even where its binaries cannot run. The layer's header
   is freestanding and spells everything with a `KT_` prefix, so both sets can sit in one
   translation unit. */
#include "driver_posix.h"

#include <fcntl.h>
#include <signal.h>
#include <sys/syscall.h>

#include "krusty_posix.h"

#define SAME(layer, target) _Static_assert((layer) == (target), #layer " is not " #target)

SAME(KT_O_RDONLY, O_RDONLY);
SAME(KT_O_WRONLY, O_WRONLY);
SAME(KT_O_RDWR, O_RDWR);
SAME(KT_O_CREAT, O_CREAT);
SAME(KT_O_TRUNC, O_TRUNC);
SAME(KT_O_APPEND, O_APPEND);
SAME(KT_O_CLOEXEC, O_CLOEXEC);
SAME(KT_O_DIRECTORY, O_DIRECTORY);

SAME(KT_AT_FDCWD, AT_FDCWD);
SAME(KT_AT_SYMLINK_NOFOLLOW, AT_SYMLINK_NOFOLLOW);
SAME(KT_AT_REMOVEDIR, AT_REMOVEDIR);
SAME(KT_AT_EMPTY_PATH, AT_EMPTY_PATH);

SAME(KT_F_DUPFD, F_DUPFD);
SAME(KT_F_GETFD, F_GETFD);
SAME(KT_F_SETFD, F_SETFD);
SAME(KT_F_GETFL, F_GETFL);
SAME(KT_F_SETFL, F_SETFL);
SAME(KT_F_GETLK, F_GETLK);
SAME(KT_F_SETLK, F_SETLK);
SAME(KT_F_SETLKW, F_SETLKW);
SAME(KT_F_SETOWN, F_SETOWN);
SAME(KT_F_GETOWN, F_GETOWN);
SAME(KT_F_DUPFD_CLOEXEC, F_DUPFD_CLOEXEC);

SAME(KT_EINTR, EINTR);
SAME(KT_EAGAIN, EAGAIN);
SAME(KT_EBADF, EBADF);
SAME(KT_ENOMEM, ENOMEM);
SAME(KT_EBUSY, EBUSY);
SAME(KT_EINVAL, EINVAL);
SAME(KT_ETIMEDOUT, ETIMEDOUT);

SAME(KT_SC_CLK_TCK, _SC_CLK_TCK);
SAME(KT_SC_OPEN_MAX, _SC_OPEN_MAX);
SAME(KT_SC_PAGESIZE, _SC_PAGESIZE);
SAME(KT_SC_NPROCESSORS_CONF, _SC_NPROCESSORS_CONF);
SAME(KT_SC_NPROCESSORS_ONLN, _SC_NPROCESSORS_ONLN);

SAME(KT_SA_RESTART, SA_RESTART);
#ifdef SA_RESTORER
SAME(KT_SA_RESTORER, SA_RESTORER);
#endif

SAME(KT_SYS_READ, SYS_read);
SAME(KT_SYS_WRITE, SYS_write);
SAME(KT_SYS_MMAP, SYS_mmap);
SAME(KT_SYS_MUNMAP, SYS_munmap);
SAME(KT_SYS_MPROTECT, SYS_mprotect);
SAME(KT_SYS_EXIT, SYS_exit_group);
SAME(KT_SYS_EXIT_THREAD, SYS_exit);
SAME(KT_SYS_CLONE, SYS_clone);
SAME(KT_SYS_GETTID, SYS_gettid);
SAME(KT_SYS_FUTEX, SYS_futex);
SAME(KT_SYS_CLOSE, SYS_close);
SAME(KT_SYS_LSEEK, SYS_lseek);
SAME(KT_SYS_RT_SIGACTION, SYS_rt_sigaction);
SAME(KT_SYS_RT_SIGPROCMASK, SYS_rt_sigprocmask);
SAME(KT_SYS_RT_SIGRETURN, SYS_rt_sigreturn);
SAME(KT_SYS_SCHED_YIELD, SYS_sched_yield);
SAME(KT_SYS_NANOSLEEP, SYS_nanosleep);
SAME(KT_SYS_GETPID, SYS_getpid);
SAME(KT_SYS_SOCKET, SYS_socket);
SAME(KT_SYS_CONNECT, SYS_connect);
SAME(KT_SYS_SENDTO, SYS_sendto);
SAME(KT_SYS_RECVFROM, SYS_recvfrom);
SAME(KT_SYS_SHUTDOWN, SYS_shutdown);
SAME(KT_SYS_BIND, SYS_bind);
SAME(KT_SYS_LISTEN, SYS_listen);
SAME(KT_SYS_GETSOCKNAME, SYS_getsockname);
SAME(KT_SYS_GETPEERNAME, SYS_getpeername);
SAME(KT_SYS_SETSOCKOPT, SYS_setsockopt);
SAME(KT_SYS_GETSOCKOPT, SYS_getsockopt);
SAME(KT_SYS_ACCEPT4, SYS_accept4);
SAME(KT_SYS_KILL, SYS_kill);
SAME(KT_SYS_TGKILL, SYS_tgkill);
SAME(KT_SYS_FCNTL, SYS_fcntl);
SAME(KT_SYS_SCHED_GETAFFINITY, SYS_sched_getaffinity);
SAME(KT_SYS_GETDENTS64, SYS_getdents64);
SAME(KT_SYS_CLOCK_GETTIME, SYS_clock_gettime);
SAME(KT_SYS_EPOLL_CREATE1, SYS_epoll_create1);
SAME(KT_SYS_EPOLL_CTL, SYS_epoll_ctl);
SAME(KT_SYS_EPOLL_PWAIT, SYS_epoll_pwait);
SAME(KT_SYS_OPENAT, SYS_openat);
SAME(KT_SYS_MKDIRAT, SYS_mkdirat);
SAME(KT_SYS_NEWFSTATAT, SYS_newfstatat);
SAME(KT_SYS_UNLINKAT, SYS_unlinkat);
SAME(KT_SYS_PIPE2, SYS_pipe2);
#ifdef SYS_arch_prctl
SAME(KT_SYS_ARCH_PRCTL, SYS_arch_prctl);
#endif

void kt_program_entry(void) { driver_say("OK\n"); }
