/* Threads that foreign code starts run Kotlin on one heap, and a collection on any of them keeps
   what every other one holds.

   Each worker attaches through `kt_callback_enter`, as a thread a C library started would, and
   repeatedly builds a list it holds only in its own frame, allocates enough garbage beside it that
   collections run (on whichever thread happens to allocate), and steps out to "foreign code" with
   `kt_native_enter`, so the others get the lock while its list is reachable from nothing but its
   stack and the registers it recorded. Back in, the list must be intact. Every round, each worker
   also pushes onto one list hanging off a global root, so the threads share objects too. The main
   thread does the same work as a worker before joining them, and after the join the stacks are
   unmapped: a collection then must not scan a thread that has left. */
#include "driver_threads.h"

typedef struct Node {
    KObjectHeader header;
    struct Node *next;
    kt_long value;
} Node;

static const uint32_t node_references[] = {offsetof(Node, next)};
static const KType node_type = {
    .name = "Node",
    .name_length = 4,
    .instance_size = sizeof(Node),
    .reference_count = 1,
    .reference_offsets = node_references,
};

#define WORKERS 4
#define ROUNDS 30
#define KEPT 300
#define GARBAGE 3000

static Node *shared;

static Node *node(Node *next, kt_long value) {
    Node *made = (Node *)kt_gc_allocate(&node_type, sizeof(Node));
    made->next = next;
    made->value = value;
    return made;
}

static kt_long value_of(long worker, int round, int index) {
    return (kt_long)worker * 1000000 + round * 1000 + index;
}

__attribute__((noinline)) static Node *build(long worker, int round) {
    Node *list = NULL;
    for (int index = 0; index < KEPT; index++) {
        list = node(list, value_of(worker, round, index));
        if (index % 10 == 0) {
            for (int junk = 0; junk < GARBAGE / (KEPT / 10); junk++) {
                (void)node(NULL, -1);
            }
        }
    }
    return list;
}

__attribute__((noinline)) static void check(Node *list, long worker, int round) {
    int index = KEPT;
    for (Node *at = list; at != NULL; at = at->next) {
        index--;
        if (at->header.type != &node_type || at->value != value_of(worker, round, index)) {
            KT_SYS_FAIL("a list its thread still held was collected or overwritten\n");
        }
    }
    if (index != 0) {
        KT_SYS_FAIL("a list its thread still held lost nodes\n");
    }
}

static void work(long worker) {
    for (int round = 0; round < ROUNDS; round++) {
        Node *mine = build(worker, round);
        KThread *self = kt_native_enter();
        kt_syscall(KT_SYS_SCHED_YIELD, 0, 0, 0, 0, 0, 0);
        kt_native_leave(self);
        check(mine, worker, round);
        shared = node(shared, value_of(worker, round, 0));
    }
}

static void worker_routine(void *argument) {
    uintptr_t bottom = 0;
    KThreadEntry entry;
    kt_callback_enter(&entry, &bottom);
    work((long)argument);
    kt_callback_leave(&entry);
}

static void check_shared(void) {
    int seen[WORKERS + 1] = {0};
    for (Node *at = shared; at != NULL; at = at->next) {
        long worker = (long)(at->value / 1000000);
        if (at->header.type != &node_type || worker < 0 || worker > WORKERS) {
            KT_SYS_FAIL("the shared list holds a node no thread pushed\n");
        }
        seen[worker]++;
    }
    for (int worker = 0; worker <= WORKERS; worker++) {
        if (seen[worker] != ROUNDS) {
            KT_SYS_FAIL("the shared list lost a node\n");
        }
    }
}

void kt_program_entry(void) {
    uintptr_t bottom = 0;
    kt_runtime_init(&bottom);
    kt_gc_add_global_root((void **)&shared);

    DriverThread threads[WORKERS];
    for (long worker = 0; worker < WORKERS; worker++) {
        driver_thread_start(&threads[worker], worker_routine, (void *)worker);
    }
    work(WORKERS);
    for (int worker = 0; worker < WORKERS; worker++) {
        driver_thread_join(&threads[worker]);
    }
    check_shared();
    kt_gc_collect();
    check_shared();
    kt_sys_write(1, "OK\n", 3);
}
