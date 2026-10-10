/* krusty native runtime, POSIX layer: threads. The thread pointer and `errno`, `pthread_create`
   and `pthread_join` on the runtime's own `clone`, and mutexes and condition variables on futexes.
   Hand-written freestanding C; see krusty_posix.h. */
#include "krusty_posix.h"

/* ---- the thread pointer ------------------------------------------------------------------------ */

void kt_os_thread_set(KOsThread *block) {
    block->self = block;
#if defined(__x86_64__)
    /* arch_prctl(ARCH_SET_FS, block) */
    kt_syscall(KT_SYS_ARCH_PRCTL, 0x1002, (long)block, 0, 0, 0, 0);
#elif defined(__aarch64__)
    __asm__ volatile("msr tpidr_el0, %0" : : "r"(block) : "memory");
#else
    __asm__ volatile("mv tp, %0" : : "r"(block) : "memory");
#endif
}

KOsThread *kt_os_thread_self(void) {
    KOsThread *block;
#if defined(__x86_64__)
    /* Every thread the layer knows has `%fs` set before it runs a line of the program, the first
       one in `_start`; the word there is the block's own address. */
    __asm__("movq %%fs:0, %0" : "=r"(block));
#elif defined(__aarch64__)
    __asm__("mrs %0, tpidr_el0" : "=r"(block));
#else
    __asm__("mv %0, tp" : "=r"(block));
#endif
    return block;
}

/* The program's first thread's block. */
static KOsThread kt_main_os_thread = {.running = 1};

/* What `errno` is on a thread with no block of its own: shared, so wrong only for a thread that
   foreign code started without the layer and that races another such thread. */
static int kt_shared_error;

int *__errno_location(void) {
    KOsThread *self = kt_os_thread_self();
    return self != NULL ? &self->error : &kt_shared_error;
}

/* Whether the program's first thread has its block: until it does, there is no thread pointer to
   read (on x86_64 reading one would fault). */
static bool kt_main_os_thread_set;

/* The runtime's hooks: the program's first thread and every thread `kt_thread_start` makes get a
   block of their own. These replace the runtime's do-nothing defaults. */
void kt_os_thread_begin(void) {
    if (!kt_main_os_thread_set) {
        kt_main_os_thread_set = true;
        kt_os_thread_set(&kt_main_os_thread);
        return;
    }
    /* A thread `kt_thread_start` cloned: it inherited its parent's pointer, and needs its own. */
    KOsThread *block = (KOsThread *)kt_map(sizeof(KOsThread));
    block->running = 1;
    kt_os_thread_set(block);
}

void kt_os_thread_end(void) {
    KOsThread *self = kt_os_thread_self();
    if (self != &kt_main_os_thread && self->mapping == NULL) {
        /* Nothing reads the thread pointer between here and the thread's end. */
        kt_unmap(self, sizeof(KOsThread));
    }
}

/* ---- pthread_create and pthread_join ----------------------------------------------------------- */

#define KT_PTHREAD_STACK_BYTES ((size_t)8 * 1024 * 1024)
#define KT_PAGE_BYTES ((size_t)4096)

/* glibc's pthread_attr_t is opaque and larger than this; the layer keeps its own two fields in
   its first bytes. Zero means "the default" for both, which is what `pthread_attr_init` writes. */
typedef struct {
    size_t stack_bytes;
    int detached;
} KThreadAttributes;

int pthread_attr_init(void *attributes) {
    KThreadAttributes *own = (KThreadAttributes *)attributes;
    own->stack_bytes = 0;
    own->detached = 0;
    return 0;
}

int pthread_attr_destroy(void *attributes) {
    (void)attributes;
    return 0;
}

int pthread_attr_setstacksize(void *attributes, size_t bytes) {
    if (bytes < 16384) {
        return KT_EINVAL;
    }
    ((KThreadAttributes *)attributes)->stack_bytes = bytes;
    return 0;
}

int pthread_attr_setdetachstate(void *attributes, int state) {
    if (state != 0 && state != 1) {
        return KT_EINVAL;
    }
    ((KThreadAttributes *)attributes)->detached = state;
    return 0;
}

/* Detached threads that have ended, or are about to: a thread cannot unmap the stack it runs on,
   so the next `pthread_create` frees the mappings of those the kernel has finished with. */
static KOsThread *kt_finished_threads;

static void kt_free_os_thread(KOsThread *thread) { kt_unmap(thread->mapping, thread->mapping_bytes); }

static void kt_queue_finished(KOsThread *thread);

static void kt_reap_finished_threads(void) {
    KOsThread *finished = __atomic_exchange_n(&kt_finished_threads, NULL, __ATOMIC_ACQUIRE);
    while (finished != NULL) {
        KOsThread *next = finished->next_finished;
        if (__atomic_load_n(&finished->running, __ATOMIC_ACQUIRE) == 0) {
            kt_free_os_thread(finished);
        } else {
            /* Still on its way out: back on the list for a later look. */
            kt_queue_finished(finished);
        }
        finished = next;
    }
}

static void kt_queue_finished(KOsThread *thread) {
    thread->next_finished = __atomic_load_n(&kt_finished_threads, __ATOMIC_RELAXED);
    while (!__atomic_compare_exchange_n(&kt_finished_threads, &thread->next_finished, thread, true,
                                        __ATOMIC_RELEASE, __ATOMIC_RELAXED)) {
    }
}

__attribute__((noreturn)) static void kt_os_thread_finish(KOsThread *self, void *result) {
    self->result = result;
    uint32_t joinable = KT_OS_THREAD_JOINABLE;
    if (!__atomic_compare_exchange_n(&self->ownership, &joinable, KT_OS_THREAD_FINISHED, false,
                                     __ATOMIC_ACQ_REL, __ATOMIC_ACQUIRE)) {
        /* Detached first: nobody will join it, so the next `pthread_create` frees it. */
        kt_queue_finished(self);
    }
    kt_sys_exit_thread();
}

__attribute__((noreturn)) static void kt_os_thread_main(void *start) {
    KOsThread *self = (KOsThread *)start;
    kt_os_thread_set(self);
    kt_os_thread_finish(self, self->routine(self->argument));
}

int pthread_create(KOsThread **thread, const void *attributes, void *(*routine)(void *),
                   void *argument) {
    kt_reap_finished_threads();
    const KThreadAttributes *own = (const KThreadAttributes *)attributes;
    size_t stack_bytes = own != NULL && own->stack_bytes != 0 ? own->stack_bytes
                                                               : KT_PTHREAD_STACK_BYTES;
    size_t mapping_bytes = (stack_bytes + sizeof(KOsThread) + KT_PAGE_BYTES - 1) &
                           ~(KT_PAGE_BYTES - 1);
    long mapped = kt_syscall(KT_SYS_MMAP, 0, (long)mapping_bytes, 3, 0x22, -1, 0);
    if ((unsigned long)mapped > (unsigned long)-4096L) {
        return (int)-mapped;
    }
    /* The block at the top of the mapping, the stack growing down from just below it. */
    KOsThread *block = (KOsThread *)((char *)mapped + mapping_bytes - sizeof(KOsThread));
    block->mapping = (void *)mapped;
    block->mapping_bytes = mapping_bytes;
    block->routine = routine;
    block->argument = argument;
    block->ownership = own != NULL && own->detached != 0 ? KT_OS_THREAD_DETACHED
                                                         : KT_OS_THREAD_JOINABLE;
    block->running = 1;
    void *stack_top = (void *)((uintptr_t)block & ~(uintptr_t)15);
    long started = kt_clone(KT_CLONE_THREAD_FLAGS, stack_top, &block->running, kt_os_thread_main,
                            block);
    if (started < 0) {
        kt_unmap((void *)mapped, mapping_bytes);
        return (int)-started;
    }
    *thread = block;
    return 0;
}

static void kt_wait_for_end(KOsThread *thread) {
    uint32_t running;
    while ((running = __atomic_load_n(&thread->running, __ATOMIC_ACQUIRE)) != 0) {
        kt_sys_futex_wait_shared(&thread->running, running);
    }
}

int pthread_join(KOsThread *thread, void **result) {
    kt_wait_for_end(thread);
    if (result != NULL) {
        *result = thread->result;
    }
    kt_free_os_thread(thread);
    return 0;
}

int pthread_detach(KOsThread *thread) {
    uint32_t joinable = KT_OS_THREAD_JOINABLE;
    if (!__atomic_compare_exchange_n(&thread->ownership, &joinable, KT_OS_THREAD_DETACHED, false,
                                     __ATOMIC_ACQ_REL, __ATOMIC_ACQUIRE)) {
        /* Its routine returned first, so it did not queue itself: free it here, once the kernel
           is done with its stack. */
        kt_wait_for_end(thread);
        kt_free_os_thread(thread);
    }
    return 0;
}

KOsThread *pthread_self(void) { return kt_os_thread_self(); }

int pthread_equal(KOsThread *left, KOsThread *right) { return left == right; }

__attribute__((noreturn)) void pthread_exit(void *result) {
    KOsThread *self = kt_os_thread_self();
    if (self == &kt_main_os_thread) {
        kt_sys_exit_thread();
    }
    kt_os_thread_finish(self, result);
}

/* ---- mutexes -----------------------------------------------------------------------------------

   The first word of glibc's pthread_mutex_t, which PTHREAD_MUTEX_INITIALIZER zeroes, is the lock:
   0 free, 1 held, 2 held with sleepers (Drepper's "Futexes Are Tricky", mutex2). Every mutex is
   a normal one: neither recursive nor checked for its owner. */

int pthread_mutex_init(uint32_t *mutex, const void *attributes) {
    (void)attributes;
    *mutex = 0;
    return 0;
}

int pthread_mutex_destroy(uint32_t *mutex) {
    (void)mutex;
    return 0;
}

int pthread_mutex_lock(uint32_t *mutex) {
    uint32_t seen = 0;
    if (__atomic_compare_exchange_n(mutex, &seen, 1, false, __ATOMIC_ACQUIRE, __ATOMIC_RELAXED)) {
        return 0;
    }
    if (seen != 2) {
        seen = __atomic_exchange_n(mutex, 2, __ATOMIC_ACQUIRE);
    }
    while (seen != 0) {
        kt_sys_futex_wait(mutex, 2);
        seen = __atomic_exchange_n(mutex, 2, __ATOMIC_ACQUIRE);
    }
    return 0;
}

int pthread_mutex_trylock(uint32_t *mutex) {
    uint32_t seen = 0;
    return __atomic_compare_exchange_n(mutex, &seen, 1, false, __ATOMIC_ACQUIRE, __ATOMIC_RELAXED)
               ? 0
               : KT_EBUSY;
}

int pthread_mutex_unlock(uint32_t *mutex) {
    if (__atomic_exchange_n(mutex, 0, __ATOMIC_RELEASE) == 2) {
        kt_sys_futex_wake(mutex, 1);
    }
    return 0;
}

/* ---- condition variables -----------------------------------------------------------------------

   The first word of glibc's pthread_cond_t, zeroed by PTHREAD_COND_INITIALIZER, is a sequence
   number every signal advances. A waiter reads it before it releases the mutex and sleeps only
   while it is unchanged, so a signal between the two is never lost. Waking can be spurious, which
   POSIX allows: the caller re-checks its predicate. */

int pthread_cond_init(uint32_t *condition, const void *attributes) {
    (void)attributes;
    *condition = 0;
    return 0;
}

int pthread_cond_destroy(uint32_t *condition) {
    (void)condition;
    return 0;
}

int pthread_cond_wait(uint32_t *condition, uint32_t *mutex) {
    uint32_t sequence = __atomic_load_n(condition, __ATOMIC_RELAXED);
    pthread_mutex_unlock(mutex);
    kt_sys_futex_wait(condition, sequence);
    /* Taken back as contended: another waiter may be asleep on the mutex behind this one. */
    while (__atomic_exchange_n(mutex, 2, __ATOMIC_ACQUIRE) != 0) {
        kt_sys_futex_wait(mutex, 2);
    }
    return 0;
}

int pthread_cond_signal(uint32_t *condition) {
    __atomic_fetch_add(condition, 1, __ATOMIC_RELEASE);
    kt_sys_futex_wake(condition, 1);
    return 0;
}

int pthread_cond_broadcast(uint32_t *condition) {
    __atomic_fetch_add(condition, 1, __ATOMIC_RELEASE);
    kt_sys_futex_wake(condition, 0x7fffffff);
    return 0;
}

int sched_yield(void) { return (int)kt_posix_result(kt_syscall(KT_SYS_SCHED_YIELD, 0, 0, 0, 0, 0, 0)); }
