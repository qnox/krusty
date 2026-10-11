/* Threads through the POSIX layer: `pthread_create` and `pthread_join` with a result, a mutex
   many threads contend for, a condition variable, detached threads, `errno` that is each
   thread's own, and `raise`, which signals the thread that calls it. */
#include "driver_posix.h"

#include <fcntl.h>
#include <pthread.h>
#include <signal.h>

#define THREADS 6
#define ROUNDS 20000

static pthread_mutex_t counter_lock = PTHREAD_MUTEX_INITIALIZER;
static long counter;

static pthread_mutex_t queue_lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t queue_changed = PTHREAD_COND_INITIALIZER;
static int queued;
static int consumed;
static int detached_done;

/* Counts under the lock, and sets errno between rounds to a value of its own, which must survive
   every other thread setting theirs. */
static void *count(void *argument) {
    long index = (long)argument;
    int own_error = index % 2 == 0 ? EBADF : ENOENT;
    for (int round = 0; round < ROUNDS; round++) {
        if (round % 1000 == 0) {
            if (own_error == EBADF) {
                DRIVER_CHECK(close(-1) == -1, "close(-1) succeeded");
            } else {
                DRIVER_CHECK(open("/nonexistent/krusty", O_RDONLY) == -1, "open succeeded");
            }
            sched_yield();
            DRIVER_CHECK(errno == own_error, "another thread's errno leaked into this one");
        }
        pthread_mutex_lock(&counter_lock);
        counter++;
        pthread_mutex_unlock(&counter_lock);
    }
    DRIVER_CHECK(!pthread_equal(pthread_self(), (pthread_t)0), "pthread_self is null");
    return (void *)(index * 10);
}

static void *consume(void *argument) {
    (void)argument;
    pthread_mutex_lock(&queue_lock);
    while (consumed < 100) {
        while (queued == 0) {
            pthread_cond_wait(&queue_changed, &queue_lock);
        }
        queued--;
        consumed++;
        pthread_cond_broadcast(&queue_changed);
    }
    pthread_mutex_unlock(&queue_lock);
    return NULL;
}

static void *detached(void *argument) {
    (void)argument;
    pthread_mutex_lock(&queue_lock);
    detached_done++;
    pthread_cond_broadcast(&queue_changed);
    pthread_mutex_unlock(&queue_lock);
    return NULL;
}

static volatile pthread_t handled_on;

static void on_usr1(int number) {
    (void)number;
    handled_on = pthread_self();
}

/* Raises SIGUSR1 while the main thread waits in `pthread_join` with it unblocked: a signal sent to
   the process could run its handler on either thread, and the kernel picks the main one. */
static void *raise_here(void *argument) {
    (void)argument;
    handled_on = 0;
    if (raise(SIGUSR1) != 0) {
        return (void *)1;
    }
    return (void *)(long)(pthread_equal(handled_on, pthread_self()) ? 0 : 2);
}

static void check_raise_signals_the_calling_thread(void) {
    DRIVER_CHECK(signal(SIGUSR1, on_usr1) != SIG_ERR, "signal failed");
    pthread_t raiser;
    void *result = (void *)3;
    DRIVER_CHECK(pthread_create(&raiser, NULL, raise_here, NULL) == 0 &&
                     pthread_join(raiser, &result) == 0,
                 "the raising thread failed");
    DRIVER_CHECK(result != (void *)1, "raise failed");
    DRIVER_CHECK(result == NULL, "raise ran the handler on another thread");
    DRIVER_CHECK(signal(SIGUSR1, SIG_DFL) == on_usr1, "signal did not answer the handler");
}

void kt_program_entry(void) {
    pthread_t main_thread = pthread_self();
    errno = 0;
    DRIVER_CHECK(open("/nonexistent/krusty", O_RDONLY) == -1 && errno == ENOENT,
                 "the main thread's errno");

    pthread_t threads[THREADS];
    for (long index = 0; index < THREADS; index++) {
        DRIVER_CHECK(pthread_create(&threads[index], NULL, count, (void *)index) == 0,
                     "pthread_create failed");
        DRIVER_CHECK(!pthread_equal(threads[index], main_thread), "a new thread is the main one");
    }
    for (long index = 0; index < THREADS; index++) {
        void *result = NULL;
        DRIVER_CHECK(pthread_join(threads[index], &result) == 0, "pthread_join failed");
        DRIVER_CHECK(result == (void *)(index * 10), "pthread_join did not answer the result");
    }
    DRIVER_CHECK(counter == (long)THREADS * ROUNDS, "the mutex let increments race");
    DRIVER_CHECK(errno == ENOENT, "the threads changed the main thread's errno");
    DRIVER_CHECK(pthread_equal(pthread_self(), main_thread), "pthread_self changed");

    pthread_attr_t small;
    DRIVER_CHECK(pthread_attr_init(&small) == 0 &&
                     pthread_attr_setstacksize(&small, 64 * 1024) == 0,
                 "pthread_attr failed");
    pthread_t consumer;
    DRIVER_CHECK(pthread_create(&consumer, &small, consume, NULL) == 0, "the consumer failed");
    for (int item = 0; item < 100; item++) {
        pthread_mutex_lock(&queue_lock);
        while (queued >= 3) {
            pthread_cond_wait(&queue_changed, &queue_lock);
        }
        queued++;
        pthread_cond_signal(&queue_changed);
        pthread_mutex_unlock(&queue_lock);
    }
    DRIVER_CHECK(pthread_join(consumer, NULL) == 0 && consumed == 100 && queued == 0,
                 "the condition variable lost an item");

    /* Detached both ways; their stacks are freed by later creations, which must still work. */
    pthread_attr_t detach;
    DRIVER_CHECK(pthread_attr_init(&detach) == 0 &&
                     pthread_attr_setdetachstate(&detach, PTHREAD_CREATE_DETACHED) == 0 &&
                     pthread_attr_setstacksize(&detach, 64 * 1024) == 0,
                 "the detached attribute failed");
    for (int round = 0; round < 50; round++) {
        pthread_t ignored;
        DRIVER_CHECK(pthread_create(&ignored, &detach, detached, NULL) == 0,
                     "a detached pthread_create failed");
        pthread_t later;
        DRIVER_CHECK(pthread_create(&later, &small, detached, NULL) == 0 &&
                         pthread_detach(later) == 0,
                     "pthread_detach failed");
    }
    pthread_mutex_lock(&queue_lock);
    while (detached_done < 100) {
        pthread_cond_wait(&queue_changed, &queue_lock);
    }
    pthread_mutex_unlock(&queue_lock);
    DRIVER_CHECK(pthread_attr_destroy(&small) == 0 && pthread_attr_destroy(&detach) == 0,
                 "pthread_attr_destroy failed");
    check_raise_signals_the_calling_thread();
    driver_say("OK\n");
}
