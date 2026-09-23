/* The collector keeps every object the program can reach and frees the rest, and a freed slot is
   allocated again rather than the heap growing to hold new objects.

   A list hangs off a registered global root; a much larger heap of garbage is allocated beside it.
   Each step runs in a function of its own and the stack below it is scrubbed before collecting, so
   a stale copy of a garbage pointer in a dead frame cannot keep it alive. A few may survive in
   registers the scan must treat as roots, which is what `SLACK` allows for. */
#include "krusty_rt.h"
#include "krusty_sys.h"

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

#define KEPT 1000u
#define DROPPED 10000u
#define SLACK 16u

static Node *kept;

__attribute__((noinline)) static void build_kept(void) {
    for (unsigned i = 0; i < KEPT; i++) {
        Node *node = (Node *)kt_gc_allocate(&node_type, sizeof(Node));
        node->next = kept;
        kept = node;
    }
}

/* Garbage objects each on their own, so a stray copy of one in a register keeps only that one. */
__attribute__((noinline)) static void build_garbage(void) {
    for (unsigned i = 0; i < DROPPED; i++) {
        (void)kt_gc_allocate(&node_type, sizeof(Node));
    }
}

__attribute__((noinline)) static void scrub_stack(void) {
    volatile uintptr_t words[4096];
    for (unsigned i = 0; i < 4096; i++) {
        words[i] = 0;
    }
}

__attribute__((noinline)) static void check_kept_intact(void) {
    unsigned count = 0;
    for (Node *node = kept; node != NULL; node = node->next) {
        if (node->header.type != &node_type) {
            KT_SYS_FAIL("a reachable object was overwritten\n");
        }
        count++;
    }
    if (count != KEPT) {
        KT_SYS_FAIL("a reachable object went missing\n");
    }
}

__attribute__((noinline)) static void collect(void) {
    scrub_stack();
    kt_gc_collect();
}

void kt_program_entry(void) {
    uintptr_t bottom = 0;
    kt_runtime_init(&bottom);
    kt_gc_add_global_root((void **)&kept);

    build_kept();
    build_garbage();
    collect();
    size_t live = kt_gc_live_objects();
    if (live < KEPT) {
        KT_SYS_FAIL("the collector freed a reachable object\n");
    }
    if (live > KEPT + SLACK) {
        KT_SYS_FAIL("the collector kept unreachable objects\n");
    }
    check_kept_intact();

    size_t heap = kt_gc_heap_bytes();
    build_garbage();
    if (kt_gc_heap_bytes() != heap) {
        KT_SYS_FAIL("the heap grew while freed slots were free to reuse\n");
    }
    collect();
    check_kept_intact();

    kept = NULL;
    collect();
    if (kt_gc_live_objects() > SLACK) {
        KT_SYS_FAIL("a list no root reaches was kept\n");
    }
    kt_sys_write(1, "OK\n", 3);
}
