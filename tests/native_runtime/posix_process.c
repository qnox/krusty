/* The process through the POSIX layer: the environment, `sysconf`, signals in glibc's `struct
   sigaction` (ignored, handled, blocked), clocks and sleeping. */
#include "driver_posix.h"

#include <signal.h>
#include <stdlib.h>
#include <time.h>

static void check_the_environment(void) {
    /* The harness runs every driver with KRUSTY_DRIVER naming it. */
    const char *driver = getenv("KRUSTY_DRIVER");
    DRIVER_CHECK(driver != NULL && strcmp(driver, "posix_process") == 0,
                 "getenv did not find the driver's name");
    DRIVER_CHECK(getenv("KRUSTY_DRIVE") == NULL, "getenv matched a prefix of a name");
    DRIVER_CHECK(getenv("KRUSTY_DRIVER=posix_process") == NULL, "getenv matched a whole entry");
}

static void check_sysconf(void) {
    long processors = sysconf(_SC_NPROCESSORS_ONLN);
    DRIVER_CHECK(processors >= 1 && processors == sysconf(_SC_NPROCESSORS_CONF),
                 "sysconf counted no processors");
    long page = sysconf(_SC_PAGESIZE);
    DRIVER_CHECK(page >= 4096 && (page & (page - 1)) == 0 && page == getpagesize(),
                 "sysconf's page size is not the kernel's");
    errno = 0;
    DRIVER_CHECK(sysconf(-1) == -1 && errno == EINVAL, "an unknown sysconf name is not EINVAL");
}

/* A server ignores SIGPIPE so that writing to a peer that left fails with EPIPE instead of ending
   the process. */
static void check_an_ignored_sigpipe(void) {
    DRIVER_CHECK(signal(SIGPIPE, SIG_IGN) == SIG_DFL, "SIGPIPE's first handler is not SIG_DFL");
    int ends[2];
    DRIVER_CHECK(pipe(ends) == 0 && close(ends[0]) == 0, "a pipe without a reader");
    DRIVER_CHECK(write(ends[1], "x", 1) == -1 && errno == EPIPE,
                 "a write nobody reads is not EPIPE");
    DRIVER_CHECK(close(ends[1]) == 0, "closing the pipe");
    DRIVER_CHECK(signal(SIGPIPE, SIG_DFL) == SIG_IGN, "signal did not answer the old handler");
}

static volatile sig_atomic_t usr1_seen;
static volatile sig_atomic_t usr2_seen;
static volatile sig_atomic_t usr1_info_ok;

static void on_usr1(int number, siginfo_t *info, void *context) {
    (void)context;
    usr1_seen++;
    usr1_info_ok = number == SIGUSR1 && info->si_signo == SIGUSR1 && info->si_pid == getpid();
}

static void on_usr2(int number) { usr2_seen += number == SIGUSR2; }

static void check_handlers_and_masks(void) {
    struct sigaction action;
    memset(&action, 0, sizeof action);
    action.sa_sigaction = on_usr1;
    action.sa_flags = SA_SIGINFO | SA_RESTART;
    sigemptyset(&action.sa_mask);
    sigaddset(&action.sa_mask, SIGUSR2);
    DRIVER_CHECK(sigaction(SIGUSR1, &action, NULL) == 0, "sigaction failed");
    DRIVER_CHECK(kill(getpid(), SIGUSR1) == 0, "kill failed");
    /* A handler returns through the restorer, and the program carries on. */
    DRIVER_CHECK(usr1_seen == 1 && usr1_info_ok, "the SIGUSR1 handler did not run with its info");

    struct sigaction installed;
    DRIVER_CHECK(sigaction(SIGUSR1, NULL, &installed) == 0, "reading the action back failed");
    DRIVER_CHECK(installed.sa_sigaction == on_usr1 && (installed.sa_flags & SA_SIGINFO) != 0 &&
                     sigismember(&installed.sa_mask, SIGUSR2) == 1 &&
                     sigismember(&installed.sa_mask, SIGUSR1) == 0,
                 "the action read back is not the one installed");

    DRIVER_CHECK(signal(SIGUSR2, on_usr2) != SIG_ERR, "signal failed");
    sigset_t blocked, previous;
    sigemptyset(&blocked);
    sigaddset(&blocked, SIGUSR2);
    DRIVER_CHECK(sigprocmask(SIG_BLOCK, &blocked, &previous) == 0, "sigprocmask failed");
    DRIVER_CHECK(sigismember(&previous, SIGUSR2) == 0, "SIGUSR2 was blocked already");
    DRIVER_CHECK(raise(SIGUSR2) == 0 && usr2_seen == 0, "a blocked signal was delivered");
    DRIVER_CHECK(sigprocmask(SIG_SETMASK, &previous, NULL) == 0, "unblocking failed");
    DRIVER_CHECK(usr2_seen == 1, "an unblocked pending signal was not delivered");
    DRIVER_CHECK(sigaddset(&blocked, 0) == -1 && errno == EINVAL, "signal 0 is not EINVAL");
}

static long nanoseconds(const struct timespec *at) {
    return at->tv_sec * 1000000000L + at->tv_nsec;
}

static void check_clocks(void) {
    struct timespec before, after, wall;
    DRIVER_CHECK(clock_gettime(CLOCK_MONOTONIC, &before) == 0, "the monotonic clock failed");
    struct timespec pause = {0, 20 * 1000 * 1000};
    DRIVER_CHECK(nanosleep(&pause, NULL) == 0, "nanosleep failed");
    DRIVER_CHECK(clock_gettime(CLOCK_MONOTONIC, &after) == 0, "the monotonic clock failed");
    DRIVER_CHECK(nanoseconds(&after) - nanoseconds(&before) >= 20 * 1000 * 1000,
                 "nanosleep returned early");
    DRIVER_CHECK(clock_gettime(CLOCK_REALTIME, &wall) == 0 && wall.tv_sec > 1600000000L &&
                     time(NULL) >= wall.tv_sec,
                 "the wall clock is not now");
    DRIVER_CHECK(clock_gettime(12345, &wall) == -1 && errno == EINVAL,
                 "an unknown clock is not EINVAL");
}

void kt_program_entry(void) {
    check_the_environment();
    check_sysconf();
    check_an_ignored_sigpipe();
    check_handlers_and_masks();
    check_clocks();
    driver_say("OK\n");
    exit(0);
}
