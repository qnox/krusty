/* PLATFORM-SPECIFIC: the Native embedding lifetime/collector contract, not Kotlin syntax.
   Foreign routines declare their complete OS-thread lifetime through driver_threads.h. Object
   slots must outlive each callback, remain roots while no callback is active, and disappear when
   that routine ends. No raw host without that registration is claimed to have persistent state. */
#include "driver_threads.h"

typedef struct Node {
    KObjectHeader header;
    struct Node *next;
    kt_long value;
} Node;

static const uint32_t references[] = {offsetof(Node, next)};
static const KType node_type = {
    .name = "ThreadObjectNode",
    .name_length = 16,
    .instance_size = sizeof(Node),
    .reference_count = 1,
    .reference_offsets = references,
};

#define WORKERS 2
#define KEPT 128

static const char declaration;
static uintptr_t slots[WORKERS];
static uint32_t ready;
static uint32_t proceed;
static unsigned initialized[WORKERS];

static Node *make(Node *next, kt_long value) {
    Node *node = (Node *)kt_gc_allocate(&node_type, sizeof(Node));
    node->next = next;
    node->value = value;
    return node;
}

static void check(Node *node, long worker) {
    for (int index = KEPT - 1; index >= 0; index--) {
        if (node == NULL || node->header.type != &node_type ||
            node->value != worker * 1000 + index) {
            KT_SYS_FAIL("a per-thread object was collected or changed\n");
        }
        node = node->next;
    }
    if (node != NULL) {
        KT_SYS_FAIL("a per-thread object gained nodes\n");
    }
}

static void worker(void *argument) {
    long index = (long)argument;
    uintptr_t bottom = 0;
    KThreadEntry entry;
    kt_callback_enter(&entry, &bottom);
    KRef *slot = kt_thread_object_slot(&declaration);
    if (*slot != NULL) {
        KT_SYS_FAIL("a new OS-thread lifetime inherited an object\n");
    }
    slots[index] = (uintptr_t)slot;
    Node *list = NULL;
    for (int item = 0; item < KEPT; item++) {
        list = make(list, index * 1000 + item);
    }
    *slot = (KRef)list;
    initialized[index]++;
    kt_callback_leave(&entry);
    __atomic_fetch_add(&ready, 1, __ATOMIC_RELEASE);

    /* No callback stack is a root here. The main thread collects before permitting reentry. */
    while (__atomic_load_n(&proceed, __ATOMIC_ACQUIRE) == 0) {
        kt_syscall(KT_SYS_SCHED_YIELD, 0, 0, 0, 0, 0, 0);
    }
    kt_callback_enter(&entry, &bottom);
    slot = kt_thread_object_slot(&declaration);
    if ((uintptr_t)slot != slots[index] || initialized[index] != 1) {
        KT_SYS_FAIL("callback leave reset the registered thread's object slot\n");
    }
    check((Node *)*slot, index);
    /* Nested callbacks on an already released Kotlin frame keep the same lifetime too. */
    KThread *self = kt_native_enter();
    KThreadEntry nested;
    kt_callback_enter(&nested, &bottom);
    if (kt_thread_object_slot(&declaration) != slot) {
        KT_SYS_FAIL("a nested callback changed the object slot\n");
    }
    kt_callback_leave(&nested);
    kt_native_leave(self);
    kt_callback_leave(&entry);
}

static void managed_worker(KRef argument) {
    (void)argument;
    KRef *slot = kt_thread_object_slot(&declaration);
    if (*slot != NULL) {
        KT_SYS_FAIL("a runtime-owned thread inherited an object\n");
    }
    *slot = (KRef)make(NULL, 99);
    kt_gc_collect();
    if (((Node *)*slot)->value != 99) {
        KT_SYS_FAIL("a running thread's object slot was not traced\n");
    }
}

void kt_program_entry(void) {
    uintptr_t bottom = 0;
    kt_runtime_init(&bottom);
    size_t baseline = kt_gc_live_objects();
    size_t global_roots = kt_gc_global_roots();
    KRef *main_slot = kt_thread_object_slot(&declaration);
    if (*main_slot != NULL) {
        KT_SYS_FAIL("a new managed main-thread slot was not empty\n");
    }
    *main_slot = (KRef)make(NULL, 42);

    DriverThread workers[WORKERS];
    for (long index = 0; index < WORKERS; index++) {
        driver_thread_start(&workers[index], worker, (void *)index);
    }
    while (__atomic_load_n(&ready, __ATOMIC_ACQUIRE) != WORKERS) {
        KThread *self = kt_native_enter();
        kt_syscall(KT_SYS_SCHED_YIELD, 0, 0, 0, 0, 0, 0);
        kt_native_leave(self);
    }
    if (slots[0] == slots[1] || slots[0] == (uintptr_t)main_slot ||
        slots[1] == (uintptr_t)main_slot) {
        KT_SYS_FAIL("two OS threads share a declaration slot\n");
    }
    kt_gc_collect();
    if (kt_gc_live_objects() != baseline + 1 + WORKERS * KEPT) {
        KT_SYS_FAIL("dormant thread slots did not retain exactly their object graphs\n");
    }
    __atomic_store_n(&proceed, 1, __ATOMIC_RELEASE);
    for (int index = 0; index < WORKERS; index++) {
        driver_thread_join(&workers[index]);
    }
    kt_gc_collect();
    if (kt_gc_live_objects() != baseline + 1) {
        KT_SYS_FAIL("ended foreign thread lifetimes retained object roots\n");
    }

    KThreadHandle *managed = kt_thread_start(managed_worker, NULL);
    kt_thread_join(managed);
    kt_gc_collect();
    if (kt_gc_live_objects() != baseline + 1 || kt_gc_global_roots() != global_roots ||
        kt_thread_object_slot(&declaration) != main_slot || ((Node *)*main_slot)->value != 42) {
        KT_SYS_FAIL("managed lifetime cleanup or main-thread identity changed\n");
    }
    kt_sys_write(1, "OK\n", 3);
}
