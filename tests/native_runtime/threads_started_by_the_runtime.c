/* Threads the runtime starts itself, as a static program with no C library starts them.

   `kt_thread_start` registers the new thread before it exists, so the argument it is handed stays
   alive through collections that run before it first does; the main thread forces several, with
   every reference to the arguments dropped from its own frames. Each thread then builds and checks
   a list through collections on any thread and stepping out with `kt_native_enter`, as the threads
   foreign code starts do, and `kt_thread_join` waits with the lock released. */
#include "krusty_rt.h"
#include "krusty_sys.h"

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

#define THREADS 4
#define ROUNDS 20
#define KEPT 200

static uint32_t finished;

static Node *node(Node *next, kt_long value) {
    Node *made = (Node *)kt_gc_allocate(&node_type, sizeof(Node));
    made->next = next;
    made->value = value;
    return made;
}

/* The argument a thread is started with: a list of `index + 1` nodes, each holding `index`. */
__attribute__((noinline)) static Node *argument_for(kt_long index) {
    Node *list = NULL;
    for (kt_long count = 0; count <= index; count++) {
        list = node(list, index);
    }
    return list;
}

__attribute__((noinline)) static void churn(int nodes) {
    for (int index = 0; index < nodes; index++) {
        (void)node(NULL, -1);
    }
}

__attribute__((noinline)) static void routine(KRef argument) {
    Node *given = (Node *)argument;
    kt_long index = given->value;
    kt_int length = 0;
    for (Node *at = given; at != NULL; at = at->next) {
        if (at->header.type != &node_type || at->value != index) {
            KT_SYS_FAIL("a started thread's argument was collected before it ran\n");
        }
        length++;
    }
    if (length != index + 1) {
        KT_SYS_FAIL("a started thread's argument lost nodes\n");
    }
    for (int round = 0; round < ROUNDS; round++) {
        Node *mine = NULL;
        for (int count = 0; count < KEPT; count++) {
            mine = node(mine, index * 1000 + round);
            churn(10);
        }
        KThread *self = kt_native_enter();
        kt_native_leave(self);
        for (Node *at = mine; at != NULL; at = at->next) {
            if (at->header.type != &node_type || at->value != index * 1000 + round) {
                KT_SYS_FAIL("a started thread's list was collected\n");
            }
        }
    }
    __atomic_fetch_add(&finished, 1, __ATOMIC_RELAXED);
}

__attribute__((noinline)) static void start_all(KThreadHandle **handles) {
    for (kt_long index = 0; index < THREADS; index++) {
        handles[index] = kt_thread_start(routine, (KRef)argument_for(index));
    }
}

__attribute__((noinline)) static void scrub_stack(void) {
    volatile uintptr_t words[1024];
    for (unsigned i = 0; i < 1024; i++) {
        words[i] = 0;
    }
    (void)words[0];
}

void kt_program_entry(void) {
    uintptr_t bottom = 0;
    kt_runtime_init(&bottom);
    KThreadHandle *handles[THREADS];
    /* The lock is still held: no started thread has run yet, and only the registry keeps the
       arguments alive through these collections. */
    start_all(handles);
    scrub_stack();
    for (int round = 0; round < 3; round++) {
        churn(200000);
        kt_gc_collect();
    }
    for (int index = 0; index < THREADS; index++) {
        kt_thread_join(handles[index]);
    }
    if (__atomic_load_n(&finished, __ATOMIC_RELAXED) != THREADS) {
        KT_SYS_FAIL("a started thread did not finish its routine\n");
    }
    kt_gc_collect();
    kt_sys_write(1, "OK\n", 3);
}
