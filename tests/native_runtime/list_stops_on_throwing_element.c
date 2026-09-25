/* A list whose element's `equals`, `hashCode` or `toString` THROWS stops where it threw, as
   Kotlin's does: those members are ordinary loops over the elements, so the exception leaves the
   loop at once and no later element is asked anything. `kt_throw` records the exception and comes
   back, and the element's member then returns a placeholder; the walks used to take that
   placeholder as the answer and carry on -- `indexOf` comparing the elements after it, `hashCode`
   hashing them, `toString` and `joinToString` rendering the failed text as `null` and then asking
   every later element for its own, and `equals` believing a placeholder `true`.

   Every walk below is handed three elements, of which the MIDDLE one throws, so a walk from either
   end asks a quiet element first and has one left after the thrower. Each must end with the
   thrower's exception pending and the thrower's member the last call it made into the program.

   The walks over any `Iterable` are handed one the PROGRAM implements, whose iterator's `hasNext`
   and `next` are calls into the program too. A walk over the runtime's own list would hide one
   that carried on: its next step asks the list iterator for an element, finds the exception
   pending there and ends, having called nothing a program can see. */
#include "collections_later_tiers.h"

/* An element that knows which one it is, so a walk's last callback can be named. */
typedef struct Hostile {
    KObjectHeader header;
    kt_int id;
} Hostile;

/* The id of the element whose members throw; zero while the driver builds what it then checks. */
static kt_int thrower;
/* What the thrower's `equals` returns after recording its exception. A program's `equals` can
   return anything once it has thrown, and a walk must not act on either answer. */
static kt_boolean placeholder;
static int calls;
/* The element whose member was the last call into the program; zero after an iterator member. */
static kt_int last_called;

/* Count the call and answer whether this element is the one that throws, having thrown. */
static kt_boolean called(KRef self) {
    kt_int id = ((const Hostile *)self)->id;
    calls++;
    last_called = id;
    if (id != thrower) {
        return 0;
    }
    kt_throw(kt_throwable_new(&kt_type_illegal_argument_exception, NULL));
    return 1;
}

static kt_boolean hostile_equals(KRef self, KRef other) {
    if (called(self)) {
        return placeholder;
    }
    return self == other;
}

static kt_int hostile_hash_code(KRef self) {
    if (called(self)) {
        return 0;
    }
    return ((const Hostile *)self)->id;
}

static KRef hostile_to_string(KRef self) {
    if (called(self)) {
        return NULL;
    }
    return kt_string_utf8("h", 1);
}

static const kt_fn hostile_vtable[] = {(kt_fn)hostile_equals, (kt_fn)hostile_hash_code,
                                       (kt_fn)hostile_to_string};

static const KType hostile_type = {
    .name = "Hostile",
    .name_length = sizeof("Hostile") - 1,
    .instance_size = sizeof(Hostile),
    .super = &kt_type_any,
    .vtable = hostile_vtable,
    .vtable_length = 3,
};

static KRef hostile(kt_int id) {
    Hostile *element = (Hostile *)kt_gc_allocate(&hostile_type, sizeof(Hostile));
    element->id = id;
    return (KRef)element;
}

static KRef list;
static KRef array;
static KRef probe;
static KRef program_iterable;

/* A class of the program implementing `Iterable`, whose iterator walks `list` by index. */
typedef struct ProgramIterator {
    KObjectHeader header;
    kt_int at;
} ProgramIterator;

static kt_boolean program_has_next(KRef self) {
    last_called = 0;
    return ((const ProgramIterator *)self)->at < kt_list_size(list);
}

static KRef program_next(KRef self) {
    last_called = 0;
    return kt_list_get(list, ((ProgramIterator *)self)->at++);
}

static const KType program_iterator_type = {
    .name = "ProgramIterator",
    .name_length = sizeof("ProgramIterator") - 1,
    .instance_size = sizeof(ProgramIterator),
    .super = &kt_type_any,
    .walk_has_next = program_has_next,
    .walk_next = program_next,
};

static KRef program_iterator(KRef self) {
    (void)self;
    ProgramIterator *iterator =
        (ProgramIterator *)kt_gc_allocate(&program_iterator_type, sizeof(ProgramIterator));
    iterator->at = 0;
    return (KRef)iterator;
}

static const KType program_iterable_type = {
    .name = "ProgramIterable",
    .name_length = sizeof("ProgramIterable") - 1,
    .instance_size = sizeof(KObjectHeader),
    .super = &kt_type_any,
    .walk_iterator = program_iterator,
};

/* A fresh `Array<Any?>` of the list's three elements, as a vararg call packs one. */
static KRef elements_of_list(void) {
    KRef copy = kt_array_new(&kt_type_array, 3);
    for (kt_int i = 0; i < 3; i++) {
        ((KRef *)((KArray *)copy + 1))[i] = kt_list_get(list, i);
    }
    return copy;
}

/* Each check runs one walk from a clean slate, with the middle element armed. */
static void arm(kt_boolean answer) {
    CHECK(kt_pending_exception() == NULL, "an exception was left pending\n");
    thrower = 2;
    placeholder = answer;
    calls = 0;
    last_called = 0;
}

#define EXPECT_STOPPED(literal)                                                                    \
    do {                                                                                           \
        CHECK(last_called == 2, literal " called into the program after the member that threw\n"); \
        CHECK(calls == 2, literal " did not stop at the element that threw\n");                    \
        CHECK(took(&kt_type_illegal_argument_exception),                                           \
              literal " did not come back with the element's exception\n");                        \
        thrower = 0;                                                                               \
    } while (0)

void kt_program_entry(void) {
    int stack_bottom;
    kt_runtime_init(&stack_bottom);
    kt_gc_add_global_root((void **)&list);
    kt_gc_add_global_root((void **)&array);
    kt_gc_add_global_root((void **)&probe);
    kt_gc_add_global_root((void **)&program_iterable);
    list = kt_mutable_list_new();
    kt_mutable_list_add(list, hostile(1));
    kt_mutable_list_add(list, hostile(2));
    kt_mutable_list_add(list, hostile(3));
    array = elements_of_list();
    probe = hostile(9);
    program_iterable = (KRef)kt_gc_allocate(&program_iterable_type, sizeof(KObjectHeader));

    /* The list's own members. */
    arm(0);
    (void)kt_list_index_of(list, probe);
    EXPECT_STOPPED("indexOf");
    arm(0);
    (void)kt_list_last_index_of(list, probe);
    EXPECT_STOPPED("lastIndexOf");
    arm(0);
    (void)kt_list_contains(list, probe);
    EXPECT_STOPPED("contains");
    /* A thrower whose `equals` answers true is still no match: `remove` leaves the list whole. */
    arm(1);
    (void)kt_mutable_list_remove(list, probe);
    EXPECT_STOPPED("remove");
    CHECK(kt_list_size(list) == 3 && ((const Hostile *)kt_list_get(list, 1))->id == 2,
          "a remove whose comparison threw removed an element\n");
    /* Another list of the same elements, so `equals` asks each element about itself; the thrower
       answers true, and the comparison must not take that as a reason to go on. */
    arm(1);
    (void)kt_equals(list, kt_list_of(array));
    EXPECT_STOPPED("equals");
    arm(0);
    (void)kt_hash_code(list);
    EXPECT_STOPPED("hashCode");
    arm(0);
    (void)kt_to_string(list);
    EXPECT_STOPPED("toString");

    /* The walks over any iterable, handed the program's own. */
    arm(0);
    (void)kt_iterable_join_to_string(program_iterable);
    EXPECT_STOPPED("joinToString");
    arm(0);
    (void)kt_iterable_index_of(program_iterable, probe);
    EXPECT_STOPPED("Iterable.indexOf");
    arm(1);
    CHECK(kt_iterable_index_of(program_iterable, probe) == -1,
          "Iterable.indexOf matched an element whose equals threw\n");
    EXPECT_STOPPED("Iterable.indexOf answering true");

    /* The same members of an array's contents, which walk an array the way the list's walk its
       storage. */
    arm(1);
    (void)kt_array_content_equals(array, elements_of_list());
    EXPECT_STOPPED("contentEquals");
    arm(0);
    (void)kt_array_content_hash_code(array);
    EXPECT_STOPPED("contentHashCode");
    arm(0);
    (void)kt_array_content_to_string(array);
    EXPECT_STOPPED("contentToString");

    /* One element, one call: an `IndexedValue` whose value's `toString` threw answers no text,
       where it used to go on and answer the text with `null` in the value's place. */
    arm(0);
    KRef indexed = kt_indexed_value(0, kt_list_get(list, 1));
    CHECK(kt_to_string(indexed) == NULL, "IndexedValue.toString answered text after a throw\n");
    CHECK(calls == 1 && took(&kt_type_illegal_argument_exception),
          "IndexedValue.toString did not come back with the value's exception\n");
    thrower = 0;
    kt_sys_write(1, "OK\n", 3);
}
