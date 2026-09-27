/* The collector finds references wherever the program keeps them under their own declared types:
   a field declared as a pointer to the program's struct, an array element stored through that
   same pointer type, and a global declared as one. The collector never names those types, so it
   reads each slot's bytes rather than reading it through a `void **` or `uintptr_t *` lvalue,
   which C's effective-type rules would make undefined.

   The chain below is reachable only through those typed slots: the global holds the array, the
   array's elements hold the heads of lists, and each list is linked through `next`. Nothing else
   keeps any of it, and one unlinked node is allocated alongside so the check can tell a
   collection that keeps everything from one that keeps what is reachable. */
#include "krusty_rt.h"
#include "krusty_sys.h"

typedef struct Node {
    KObjectHeader header;
    struct Node *next;
    uintptr_t value;
} Node;

static const uint32_t node_references[] = {offsetof(Node, next)};
static const KType node_type = {
    .name = "Node",
    .name_length = 4,
    .instance_size = sizeof(Node),
    .reference_count = 1,
    .reference_offsets = node_references,
};
static const KType nodes_type = {
    .name = "Array",
    .name_length = 5,
    .instance_size = sizeof(KArray),
    .element_size = sizeof(Node *),
    .element_references = 1,
};

#define LISTS 8u
#define LENGTH 16u

typedef struct Nodes {
    KArray array;
    Node *elements[LISTS];
} Nodes;

static Nodes *lists;

__attribute__((noinline)) static void build(void) {
    lists = (Nodes *)kt_gc_allocate(&nodes_type, sizeof(Nodes));
    lists->array.length = LISTS;
    for (unsigned list = 0; list < LISTS; list++) {
        Node *head = NULL;
        for (unsigned i = 0; i < LENGTH; i++) {
            Node *node = (Node *)kt_gc_allocate(&node_type, sizeof(Node));
            node->next = head;
            node->value = list * LENGTH + i;
            head = node;
        }
        lists->elements[list] = head;
    }
    (void)kt_gc_allocate(&node_type, sizeof(Node));
}

__attribute__((noinline)) static void collect(void) {
    volatile uintptr_t words[4096];
    for (unsigned i = 0; i < 4096; i++) {
        words[i] = 0;
    }
    (void)words[0];
    kt_gc_collect();
}

void kt_program_entry(void) {
    uintptr_t bottom = 0;
    kt_runtime_init(&bottom);
    kt_gc_add_global_root((void **)&lists);
    build();
    collect();
    if (kt_gc_live_objects() != 1 + LISTS * LENGTH) {
        KT_SYS_FAIL("the collector kept something other than what the typed slots reach\n");
    }
    for (unsigned list = 0; list < LISTS; list++) {
        unsigned i = LENGTH;
        for (Node *node = lists->elements[list]; node != NULL; node = node->next) {
            i--;
            if (node->header.type != &node_type || node->value != list * LENGTH + i) {
                KT_SYS_FAIL("a node reached only through a typed slot was freed\n");
            }
        }
        if (i != 0) {
            KT_SYS_FAIL("a list reached only through typed slots lost nodes\n");
        }
    }
    kt_sys_write(1, "OK\n", 3);
}
