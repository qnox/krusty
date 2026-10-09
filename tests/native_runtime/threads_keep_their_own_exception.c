/* The exception in flight belongs to the thread that raised it.

   Generated code reads one global slot, `kt_pending`, after every call; it is the holder's, saved
   into the thread when it releases the lock and restored when it takes it back. So an exception the
   main thread has in flight when it steps out to foreign code is still its own when it returns,
   whatever other threads raised meanwhile; a callback starts with nothing in flight and hands what
   it raised to the foreign code that called it rather than leaving it pending; and a callback on a
   thread that is already attached (the C function a Kotlin frame called, calling back in) is that
   same thread, whose objects stay alive through a collection the callback makes. */
#include "driver_threads.h"

typedef struct Node {
    KObjectHeader header;
    struct Node *next;
} Node;

static const uint32_t node_references[] = {offsetof(Node, next)};
static const KType node_type = {
    .name = "Node",
    .name_length = 4,
    .instance_size = sizeof(Node),
    .reference_count = 1,
    .reference_offsets = node_references,
};

static kt_boolean worker_started_clean;
static kt_boolean worker_saw_its_own;

/* A callback that raises and catches its own exception while the main thread has one in flight. */
static void worker_routine(void *argument) {
    (void)argument;
    uintptr_t bottom = 0;
    KThreadEntry entry;
    kt_callback_enter(&entry, &bottom);
    worker_started_clean = kt_pending_exception() == NULL;
    KRef thrown = kt_throwable_new(&kt_type_illegal_state_exception, NULL);
    kt_throw(thrown);
    worker_saw_its_own = kt_pending_exception() == thrown;
    kt_clear_pending();
    kt_callback_leave(&entry);
}

__attribute__((noinline)) static Node *build(int count) {
    Node *list = NULL;
    for (int index = 0; index < count; index++) {
        Node *made = (Node *)kt_gc_allocate(&node_type, sizeof(Node));
        made->next = list;
        list = made;
    }
    return list;
}

__attribute__((noinline)) static int length(Node *list) {
    int count = 0;
    for (Node *at = list; at != NULL; at = at->next) {
        if (at->header.type != &node_type) {
            KT_SYS_FAIL("a node was collected or overwritten\n");
        }
        count++;
    }
    return count;
}

/* The C function a Kotlin frame called, calling back into Kotlin on the same thread: it raises and
   catches an exception of its own, which must not replace its caller's. */
__attribute__((noinline)) static void callback_on_this_thread(void) {
    uintptr_t bottom = 0;
    KThreadEntry entry;
    kt_callback_enter(&entry, &bottom);
    if (entry.attached) {
        KT_SYS_FAIL("a callback on an attached thread attached it again\n");
    }
    if (kt_pending_exception() != NULL) {
        KT_SYS_FAIL("a callback started with its caller's exception in flight\n");
    }
    Node *inner = build(100);
    for (int round = 0; round < 3; round++) {
        (void)build(2000);
        kt_gc_collect();
    }
    if (length(inner) != 100) {
        KT_SYS_FAIL("a callback's own list lost nodes\n");
    }
    kt_throw(kt_throwable_new(&kt_type_illegal_state_exception, NULL));
    kt_clear_pending();
    kt_callback_leave(&entry);
}

void kt_program_entry(void) {
    uintptr_t bottom = 0;
    kt_runtime_init(&bottom);
    Node *outer = build(500);
    KRef mine = kt_throwable_new(&kt_type_illegal_state_exception, NULL);
    kt_throw(mine);

    DriverThread thread;
    driver_thread_start(&thread, worker_routine, NULL);
    /* The join releases the lock with `mine` in flight. */
    driver_thread_join(&thread);
    if (kt_pending_exception() != mine) {
        KT_SYS_FAIL("the main thread's exception did not survive another thread's\n");
    }
    if (!worker_started_clean) {
        KT_SYS_FAIL("a new thread started with another thread's exception in flight\n");
    }
    if (!worker_saw_its_own) {
        KT_SYS_FAIL("a callback's exception was not its own in flight\n");
    }

    /* Out to foreign code, which calls back in on this thread. */
    KThread *self = kt_native_enter();
    callback_on_this_thread();
    kt_native_leave(self);
    if (kt_pending_exception() != mine) {
        KT_SYS_FAIL("a callback on this thread replaced its exception in flight\n");
    }
    kt_clear_pending();
    kt_gc_collect();
    if (length(outer) != 500) {
        KT_SYS_FAIL("a list held across the callback lost nodes\n");
    }
    kt_sys_write(1, "OK\n", 3);
}
