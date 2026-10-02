/* krusty native runtime: lists, and how anything is walked.

   The collection half of the runtime, in a translation unit of its own so that neither half grows
   past what one file should hold. It shares the object layout and the few helpers both halves use
   through `krusty_internal.h`; everything else it reaches is the public contract in
   `krusty_rt.h`. */
#include "krusty_internal.h"

/* ---- lists --------------------------------------------------------------------------------- */

/* `listOf(...)` as a VALUE. The elements are an ordinary `Array<T>` the list holds, which is what
   Kotlin's own `listOf(vararg)` does with the array a vararg call already built -- so the elements
   are traced by the collector through the array it already knows how to trace, and the list itself
   has exactly one reference field.

   The list is IMMUTABLE, which is what makes sharing the vararg array sound: nothing a program can
   write through reaches it. `MutableList` is not this type and is not realized here. */
static const uint32_t kt_list_offsets[] = {offsetof(KList, elements)};

/* The collection interfaces, as an `is` names them. None has instances of its own, exactly like
   `Number` and the function markers; each lists its own bases, flattened. */
#define KT_INTERFACE(identifier, simple, bases)                                                    \
    const KType identifier = {KT_NAMED("kotlin.collections.", simple),                             \
                              .instance_size = sizeof(KObjectHeader),                              \
                              .super = &kt_type_any,                                               \
                              .vtable = kt_any_vtable,                                             \
                              .vtable_length = 3,                                                  \
                              .interfaces = bases,                                                 \
                              .interface_count = sizeof(bases) / sizeof((bases)[0])};

#define KT_ROOT_INTERFACE(identifier, simple)                                                      \
    const KType identifier = {KT_NAMED("kotlin.collections.", simple),                             \
                              .instance_size = sizeof(KObjectHeader),                              \
                              .super = &kt_type_any,                                               \
                              .vtable = kt_any_vtable,                                             \
                              .vtable_length = 3};

static const KType *const kt_iterable_bases[] = {&kt_type_iterable_interface};
static const KType *const kt_mutable_iterable_bases[] = {&kt_type_iterable_interface};
static const KType *const kt_collection_bases[] = {&kt_type_iterable_interface};
static const KType *const kt_mutable_collection_bases[] = {
    &kt_type_collection_interface, &kt_type_mutable_iterable_interface,
    &kt_type_iterable_interface};
static const KType *const kt_list_bases[] = {&kt_type_collection_interface,
                                             &kt_type_iterable_interface};
static const KType *const kt_mutable_list_bases[] = {
    &kt_type_list_interface, &kt_type_mutable_collection_interface, &kt_type_collection_interface,
    &kt_type_mutable_iterable_interface, &kt_type_iterable_interface};

KT_ROOT_INTERFACE(kt_type_iterator_interface, "Iterator")
KT_ROOT_INTERFACE(kt_type_iterable_interface, "Iterable")
KT_INTERFACE(kt_type_mutable_iterable_interface, "MutableIterable", kt_mutable_iterable_bases)
KT_INTERFACE(kt_type_collection_interface, "Collection", kt_collection_bases)
KT_INTERFACE(kt_type_mutable_collection_interface, "MutableCollection",
             kt_mutable_collection_bases)
KT_INTERFACE(kt_type_list_interface, "List", kt_list_bases)
KT_INTERFACE(kt_type_mutable_list_interface, "MutableList", kt_mutable_list_bases)
KT_ROOT_INTERFACE(kt_type_random_access_interface, "RandomAccess")

#undef KT_INTERFACE
#undef KT_ROOT_INTERFACE

/* What each list class implements, transitively. The read-only list is not a `MutableList`, as
   Kotlin/Native's `listOf` answer is not; the growable one is every interface here. */
static const KType *const kt_list_interfaces[] = {
    &kt_type_list_interface, &kt_type_collection_interface, &kt_type_iterable_interface,
    &kt_type_random_access_interface};
static const KType *const kt_mutable_list_interfaces[] = {
    &kt_type_mutable_list_interface,     &kt_type_list_interface,
    &kt_type_mutable_collection_interface, &kt_type_collection_interface,
    &kt_type_mutable_iterable_interface, &kt_type_iterable_interface,
    &kt_type_random_access_interface};
static const KType *const kt_iterator_interfaces[] = {&kt_type_iterator_interface};

static kt_boolean kt_list_equals(KRef self, KRef other);
static kt_int kt_list_hash_code(KRef self);
static KRef kt_list_to_string(KRef self);

static const kt_fn kt_list_vtable[] = {(kt_fn)kt_list_equals, (kt_fn)kt_list_hash_code,
                                       (kt_fn)kt_list_to_string};

const KType kt_type_list = {KT_ANONYMOUS("kotlin.collections.List"),
                            .instance_size = sizeof(KList),
                            .reference_count = 1,
                            .reference_offsets = kt_list_offsets,
                            .super = &kt_type_any,
                            .vtable = kt_list_vtable,
                            .vtable_length = 3,
                            .interfaces = kt_list_interfaces,
                            .interface_count = sizeof(kt_list_interfaces) / sizeof(KType *)};

/* ---- a growable list ------------------------------------------------------------------------

   `ArrayList`/`MutableList`. The same shape as `KList` with a SIZE beside the storage, because the
   two differ only in whether every slot of the backing array is an element: an immutable list's
   array IS its elements, a growable one's array is capacity and `size` says how much of it counts.

   Sharing the shape is what lets `kt_list_size`, `kt_list_get` and everything built on them —
   `indexOf`, `contains`, the iterator, a `for` loop — serve both. Each asks the DESCRIPTOR which
   it is holding rather than being written twice, and the iterator keeps working unchanged because
   the cursor it holds is an index and the bound it compares against is `kt_list_size`. */
typedef struct KMutableList {
    KObjectHeader header;
    KRef elements;
    kt_int size;
    /* Structural changes so far. An iterator records this when it is made and compares on every
       `next`, which is how a list notices being written through while it is being walked — Kotlin
       raises `ConcurrentModificationException` there, and the count is the only evidence: after
       `remove` the cursor and the size can agree again and nothing else would look wrong. Counted
       on the unsigned ring, as the JVM's `modCount` wraps: only equality is ever asked of it, and a
       signed count past `Int.MAX_VALUE` would be undefined. */
    uint32_t modifications;
} KMutableList;

static const uint32_t kt_mutable_list_offsets[] = {offsetof(KMutableList, elements)};

static kt_boolean kt_list_equals(KRef self, KRef other);
static kt_int kt_list_hash_code(KRef self);
static KRef kt_list_to_string(KRef self);

/* Its `equals`/`hashCode`/`toString` are the list ones: Kotlin compares any two lists by their
   elements in order, and a `List` is equal to a `MutableList` holding the same things. */
static const kt_fn kt_mutable_list_vtable[] = {(kt_fn)kt_list_equals, (kt_fn)kt_list_hash_code,
                                               (kt_fn)kt_list_to_string};

const KType kt_type_mutable_list = {
    KT_NAMED("kotlin.collections.", "ArrayList"),
    .instance_size = sizeof(KMutableList),
    .reference_count = 1,
    .reference_offsets = kt_mutable_list_offsets,
    .super = &kt_type_any,
    .vtable = kt_mutable_list_vtable,
    .vtable_length = 3,
    .interfaces = kt_mutable_list_interfaces,
    .interface_count = sizeof(kt_mutable_list_interfaces) / sizeof(KType *)};

kt_boolean kt_is_mutable_list(KRef value) {
    return value != NULL && value->header.type == &kt_type_mutable_list;
}

/* The cursor an iterator holds is an INDEX, not a pointer: the collector may not move an object,
   but an index needs no such promise and reads the same whatever the list is. */
typedef struct KListIterator {
    KObjectHeader header;
    KRef list;
    kt_int at;
    /* The list's modification count when this iterator was made; see `KMutableList`. An immutable
       list never changes, so this stays zero and the comparison always holds. */
    uint32_t modifications;
} KListIterator;

static const uint32_t kt_list_iterator_offsets[] = {offsetof(KListIterator, list)};

/* An iterator answers `kotlin.Any`'s three members by identity, as Kotlin's own iterators do. Its
   class is Kotlin/Native's: a read-only list's is the anonymous iterator `Array.asList` answers
   (`listOf(a, b)` is `a, b` as a list), a growable one's `ArrayList`'s nested `Itr`. */
#define KT_LIST_ITERATOR_TYPE                                                                      \
    .instance_size = sizeof(KListIterator), .reference_count = 1,                                  \
    .reference_offsets = kt_list_iterator_offsets, .super = &kt_type_any, .vtable = kt_any_vtable, \
    .vtable_length = 3, .interfaces = kt_iterator_interfaces,                                      \
    .interface_count = sizeof(kt_iterator_interfaces) / sizeof(KType *)
const KType kt_type_list_iterator = {KT_ANONYMOUS("kotlin.collections.Iterator"),
                                     KT_LIST_ITERATOR_TYPE};
static const KType kt_type_array_list_iterator = {KT_NAMED("kotlin.collections.ArrayList.", "Itr"),
                                                  KT_LIST_ITERATOR_TYPE};

static kt_boolean kt_is_list_iterator(KRef value) {
    return value != NULL && (value->header.type == &kt_type_list_iterator ||
                             value->header.type == &kt_type_array_list_iterator);
}

KRef kt_list_of(KRef elements) {
    /* The allocation can collect, so the array has to be reachable across it; it is, in this
       local, which the conservative root scan reads. */
    KList *list = (KList *)kt_gc_allocate(&kt_type_list, sizeof(KList));
    list->elements = elements;
    return (KRef)list;
}

KRef kt_list_empty(void) { return kt_list_of(kt_array_new(&kt_type_array, 0)); }


/* `listOf(x)` — Kotlin's own single-element overload, which is a DIFFERENT declaration from the
   vararg one and not a vararg call of length one: `listOf(anArray)` selects it and answers a list
   holding that array. There is no array yet, so this makes the one the list needs. `value` is a
   root across the allocation the way every other local here is. */
KRef kt_list_single(KRef value) {
    KRef elements = kt_array_new(&kt_type_array, 1);
    kt_elements_of(elements)[0] = value;
    return kt_list_of(elements);
}

/* Both list shapes answer here; see the note on `KMutableList` for why they share the entry point
   rather than each having its own. A growable list's array is capacity, so its SIZE is the field. */
kt_int kt_list_size(KRef list) {
    if (kt_is_mutable_list(list)) {
        return ((const KMutableList *)list)->size;
    }
    return kt_length_of(((const KList *)list)->elements);
}

kt_boolean kt_list_is_empty(KRef list) { return kt_list_size(list) == 0; }

KRef kt_list_get(KRef list, kt_int index) {
    KRef elements = ((const KList *)list)->elements;
    kt_int size = kt_list_size(list);
    if (index < 0 || index >= size) {
        /* `kt_throw` records the exception and comes back, so the read it forbids is skipped here:
           past the storage it would answer whatever lies there as an element. */
        kt_index_out_of_bounds(index, size);
        return NULL;
    }
    return kt_elements_of(elements)[index];
}

/* `first()` and `last()`. Kotlin raises `NoSuchElementException` on an empty list, with its own
   wording, rather than answering NULL — a list of a nullable element type has a perfectly good
   NULL first element and the two must stay distinguishable. The NULL each answers after raising is
   no element: the caller finds the exception pending and never reads it. */
KRef kt_list_first(KRef list) {
    if (kt_list_size(list) == 0) {
        kt_throw(kt_throwable_new(&kt_type_no_such_element_exception,
                                  kt_string_utf8("List is empty.", 14)));
        return NULL;
    }
    return kt_elements_of(((const KList *)list)->elements)[0];
}

KRef kt_list_last(KRef list) {
    kt_int size = kt_list_size(list);
    if (size == 0) {
        kt_throw(kt_throwable_new(&kt_type_no_such_element_exception,
                                  kt_string_utf8("List is empty.", 14)));
        return NULL;
    }
    return kt_elements_of(((const KList *)list)->elements)[size - 1];
}

/* Whether an exception is in flight. `kt_throw` records one and comes back, so every walk below
   that asks an iterator for an element or calls back into emitted code -- a lambda, or an element's
   own `equals`, `hashCode` or `toString` -- looks here before it uses the answer: after a raise the
   answer is a placeholder that means nothing, and the walk has to end so the exception reaches its
   caller. Kotlin's own walks are ordinary loops, which a throw leaves at once; one that went on
   would act on the placeholder and call into the program again for the elements after it. */
static kt_boolean kt_raised(void) { return kt_pending_exception() != NULL; }

static kt_boolean kt_more(KRef iterator);

/* Where every walk's `Int` counter starts: zero, as Kotlin's do. A driver moves it near
   `Int.MAX_VALUE` to reach `kt_counter_overflowed` without walking 2^31 elements
   (`walk_count_overflow`), which is what a counter that has come that far looks like; nothing in the
   runtime writes it. */
uint32_t kt_walk_counter_origin;

/* What a walk counts: an element's index, or how many elements matched. */
typedef enum { KT_COUNTS_INDICES, KT_COUNTS_ELEMENTS } KCounter;

/* Kotlin's `checkIndexOverflow` and `checkCountOverflow`, the one check every walk's counter passes
   through: an `Int` index or count that has passed `Int.MAX_VALUE` is an `ArithmeticException`
   with Kotlin's own message, not a wrap. The walks count on the unsigned ring, from
   `kt_walk_counter_origin`, where going past the maximum is defined, and ask this whether they
   have; it raises and answers true when so. */
static kt_boolean kt_counter_overflowed(uint32_t value, KCounter counts) {
    if (value <= 0x7fffffffu) {
        return false;
    }
    if (counts == KT_COUNTS_INDICES) {
        kt_throw_index_overflow();
    } else {
        kt_throw(kt_throwable_new(&kt_type_arithmetic_exception,
                                  kt_string_utf8("Count overflow has happened.", 28)));
    }
    return true;
}

kt_int kt_list_index_of(KRef list, KRef value) {
    KRef elements = ((const KList *)list)->elements;
    kt_int length = kt_list_size(list);
    for (kt_int i = 0; i < length; i++) {
        kt_boolean equal = kt_equals(kt_elements_of(elements)[i], value);
        /* An `equals` that threw answered nothing, whatever it returned: the search ends there with
           no index, so `remove` built on it removes nothing, and no later element is compared. */
        if (kt_raised()) {
            return -1;
        }
        if (equal) {
            return i;
        }
    }
    return -1;
}

kt_int kt_list_last_index_of(KRef list, KRef value) {
    KRef elements = ((const KList *)list)->elements;
    for (kt_int i = kt_list_size(list) - 1; i >= 0; i--) {
        kt_boolean equal = kt_equals(kt_elements_of(elements)[i], value);
        if (kt_raised()) {
            return -1;
        }
        if (equal) {
            return i;
        }
    }
    return -1;
}

kt_boolean kt_list_contains(KRef list, KRef value) { return kt_list_index_of(list, value) >= 0; }

KRef kt_mutable_list_new(void) {
    KMutableList *list = (KMutableList *)kt_gc_allocate(&kt_type_mutable_list, sizeof(KMutableList));
    list->elements = kt_array_new(&kt_type_array, 0);
    list->size = 0;
    list->modifications = 0;
    return (KRef)list;
}

/* `ArrayList(initialCapacity)`. The capacity is a hint and nothing observable depends on a valid
   one, but a NEGATIVE one is an error Kotlin raises, with the JVM's wording, and a program may catch
   it. */
KRef kt_mutable_list_with_capacity(kt_int capacity) {
    if (capacity < 0) {
        KRef message = kt_string_plus(kt_string_utf8("Illegal Capacity: ", 18),
                                      kt_to_string(kt_box_int(capacity)));
        kt_throw(kt_throwable_new(&kt_type_illegal_argument_exception, message));
        return NULL;
    }
    KRef list = kt_mutable_list_new();
    if (capacity > 0) {
        ((KMutableList *)list)->elements = kt_array_new(&kt_type_array, capacity);
    }
    return list;
}

/* Grow to hold at least one more, doubling so that repeated `add` stays linear overall. */
static void kt_mutable_list_reserve(KRef self) {
    KMutableList *list = (KMutableList *)self;
    kt_int capacity = kt_length_of(list->elements);
    if (list->size < capacity) {
        return;
    }
    /* Sized in 64 bits, as `kt_string_builder_reserve` sizes its growth: doubling a capacity past
       half the largest `Int` overflows `kt_int`, which is undefined and in practice comes out
       negative -- a "negative array size" where Kotlin runs out of memory, or a replacement too
       small for the copy that follows. A list already holding as many elements as an `Int` counts
       cannot take another; short of that, a doubling too large for an `Int` asks for the one more
       slot needed, and `kt_array_new` refuses any count whose storage the allocator cannot give.
       Every refusal is out of memory, and it comes before a byte of the old storage is read. */
    kt_long needed = (kt_long)list->size + 1;
    if (needed > 0x7fffffff) {
        kt_fail_oom();
    }
    kt_long grown = capacity == 0 ? 4 : (kt_long)capacity * 2;
    if (grown > 0x7fffffff) {
        grown = needed;
    }
    /* The allocation can collect, and `self` is a root in the caller's frame, so the OLD array
       stays reachable through it until the new one is stored. */
    KRef replacement = kt_array_new(&kt_type_array, (kt_int)grown);
    kt_array_copy_into(replacement, 0, list->elements);
    list->elements = replacement;
}

/* `mutableListOf(a, b, c)` — a COPY of what the vararg call packed, not a share of it. The array
   belongs to the caller, and this list can be written through. */
KRef kt_mutable_list_of(KRef elements) {
    kt_int length = kt_length_of(elements);
    /* `elements` stays live in this parameter across both allocations. */
    KRef list = kt_mutable_list_with_capacity(length);
    kt_array_copy_into(((KMutableList *)list)->elements, 0, elements);
    ((KMutableList *)list)->size = length;
    return list;
}

kt_boolean kt_mutable_list_add(KRef self, KRef value) {
    kt_mutable_list_reserve(self);
    KMutableList *list = (KMutableList *)self;
    kt_elements_of(list->elements)[list->size++] = value;
    list->modifications++;
    /* Kotlin's `MutableList.add` answers whether the list changed, which for a list is always. */
    return true;
}

/* `list += element`. Kotlin's `plusAssign` on a mutable collection IS `add`, and it answers
   `Unit` rather than the `Boolean` `add` answers — so it is its own entry point rather than a
   result the caller has to remember to drop. */
void kt_mutable_list_plus_assign(KRef self, KRef value) { kt_mutable_list_add(self, value); }

/* `list += elements`, where the right-hand side is something to walk. Kotlin has one `plusAssign`
   per shape of that — an `Iterable`, an `Array`, a `Sequence` — and each appends every element in
   order, which is the one walk this runtime already knows how to do.

   A LIST argument is copied by index up to the size it had when the call began, because that is
   what Kotlin's `addAll` of a collection does: it takes the argument's elements out first. That is
   what makes `xs.addAll(xs)` double the list, where walking `xs` with an iterator while appending
   to it would see its own list modified at the second step. */
void kt_mutable_list_add_all(KRef self, KRef elements) {
    if (elements != NULL
        && (elements->header.type == &kt_type_list || kt_is_mutable_list(elements))) {
        kt_int count = kt_list_size(elements);
        for (kt_int at = 0; at < count; at++) {
            kt_mutable_list_add(self, kt_list_get(elements, at));
        }
        return;
    }
    KRef iterator = kt_iterable_iterator(elements);
    while (kt_more(iterator)) {
        KRef element = kt_iterator_next(iterator);
        if (kt_raised()) {
            return;
        }
        kt_mutable_list_add(self, element);
    }
}

KRef kt_mutable_list_set(KRef self, kt_int index, KRef value) {
    KMutableList *list = (KMutableList *)self;
    /* Returns straight after raising, as `add(index, e)` and `removeAt` below do: `kt_throw` records
       the exception and comes back, and the write it forbids would land outside the elements --
       over the storage's length for -1, past its end for an index beyond the capacity. Kotlin
       leaves the list untouched. */
    if (index < 0 || index >= list->size) {
        kt_index_out_of_bounds(index, list->size);
        return NULL;
    }
    KRef *slot = &kt_elements_of(list->elements)[index];
    KRef previous = *slot;
    *slot = value;
    return previous;
}

void kt_mutable_list_add_at(KRef self, kt_int index, KRef value) {
    KMutableList *list = (KMutableList *)self;
    if (index < 0 || index > list->size) {
        kt_index_out_of_bounds(index, list->size);
        return;
    }
    kt_mutable_list_reserve(self);
    KRef *elements = kt_elements_of(list->elements);
    for (kt_int at = list->size; at > index; at--) {
        elements[at] = elements[at - 1];
    }
    elements[index] = value;
    list->size++;
    list->modifications++;
}

KRef kt_mutable_list_remove_at(KRef self, kt_int index) {
    KMutableList *list = (KMutableList *)self;
    if (index < 0 || index >= list->size) {
        kt_index_out_of_bounds(index, list->size);
        return NULL;
    }
    KRef *elements = kt_elements_of(list->elements);
    KRef removed = elements[index];
    for (kt_int at = index; at + 1 < list->size; at++) {
        elements[at] = elements[at + 1];
    }
    /* Clear the vacated slot so the collector stops tracing what the list no longer holds. */
    elements[--list->size] = NULL;
    list->modifications++;
    return removed;
}

kt_boolean kt_mutable_list_remove(KRef self, KRef value) {
    kt_int at = kt_list_index_of(self, value);
    if (at < 0) {
        return false;
    }
    kt_mutable_list_remove_at(self, at);
    return true;
}

void kt_mutable_list_clear(KRef self) {
    KMutableList *list = (KMutableList *)self;
    KRef *elements = kt_elements_of(list->elements);
    for (kt_int at = 0; at < list->size; at++) {
        elements[at] = NULL;
    }
    list->size = 0;
    list->modifications++;
}

KRef kt_list_iterator(KRef list) {
    const KType *type = kt_is_mutable_list(list) ? &kt_type_array_list_iterator
                                                 : &kt_type_list_iterator;
    KListIterator *iterator = (KListIterator *)kt_gc_allocate(type, sizeof(KListIterator));
    iterator->list = list;
    iterator->at = 0;
    iterator->modifications = kt_is_mutable_list(list) ? ((const KMutableList *)list)->modifications
                                                       : 0;
    return (KRef)iterator;
}

/* `cursor < size`, as Kotlin/Native's `ArrayList` iterator answers it (`Itr.hasNext` in the
   stdlib's `ArrayList.kt`): a walk whose last element removes an element leaves the cursor past the
   end, and the walk ends there without a `ConcurrentModificationException`. The JVM's `ArrayList`
   answers `cursor != size` and goes on to a `next()` that raises; the runtime behaves as
   Kotlin/Native does, and `map_mutated_source` declares the difference. */
kt_boolean kt_list_iterator_has_next(KRef iterator) {
    const KListIterator *self = (const KListIterator *)iterator;
    return self->at < kt_list_size(self->list);
}

KRef kt_list_iterator_next(KRef iterator) {
    KListIterator *self = (KListIterator *)iterator;
    /* Checked BEFORE the bound, because that is the order the difference shows in: a `remove`
       during the walk can leave the cursor inside the shortened list, where the bound says nothing
       is wrong and Kotlin still raises. */
    if (kt_is_mutable_list(self->list) &&
        ((const KMutableList *)self->list)->modifications != self->modifications) {
        kt_throw(kt_throwable_new(&kt_type_concurrent_modification_exception, NULL));
        return NULL;
    }
    if (self->at >= kt_list_size(self->list)) {
        kt_throw(kt_throwable_new(&kt_type_no_such_element_exception, NULL));
        return NULL;
    }
    return kt_list_get(self->list, self->at++);
}

/* An `Iterable` or an `Iterator` that the generator could only type by the INTERFACE — a generic
   body, an inlined stdlib extension — may be holding either of the two things this runtime can
   iterate. The static type cannot say which, so the descriptor does.

   `next` answers a reference for the same reason the question arises: a receiver typed by the
   interface has its element type erased, so what a caller there expects is the boxed element. */
/* ---- iterating an array or a string ---------------------------------------------------------- */

/* Neither an array nor a `String` is a `kotlin.collections.Iterable`, and Kotlin still lets a
   program reach every `Iterable` member on one — through an extension, or through a `for` loop the
   frontend turns into a counted walk before this runtime sees it. What arrives here is the other
   case: the receiver kept as a value and asked for an iterator. One iterator each, because the
   element of an array is read at its own width and boxed by its own descriptor, and the "element"
   of a string is a UTF-16 unit the text does not store as one. */
typedef struct KWalk {
    KObjectHeader header;
    KRef over;
    kt_int at;
    /* For a `String` only, where the chars iterator keeps its PLACE in the text: the byte offset of
       the next unit, and the trailing surrogate owed when the last unit handed out was the leading
       half of a pair (zero when none is, as in `KUnits`). Finding unit `at` from the first byte on
       every step made walking a string quadratic in its length. A string's text never changes, so
       the place stays valid; a builder's can, and a builder is walked by index instead. */
    kt_int byte_at;
    kt_char trailing;
} KWalk;

static const uint32_t kt_walk_offsets[] = {offsetof(KWalk, over)};

/* The abstract iterators of the primitive element kinds a range does not have, which an array's
   iterator subclasses as it subclasses `IntIterator`; like those, each implements `Iterator`. */
#define KT_ABSTRACT_ITERATOR(identifier, simple)                                                   \
    const KType identifier = {KT_NAMED("kotlin.collections.", simple),                             \
                              .instance_size = sizeof(KObjectHeader),                              \
                              .super = &kt_type_any,                                               \
                              .vtable = kt_any_vtable,                                             \
                              .vtable_length = 3,                                                  \
                              .interfaces = kt_iterator_interfaces,                                \
                              .interface_count = 1};
KT_ABSTRACT_ITERATOR(kt_type_boolean_iterator, "BooleanIterator")
KT_ABSTRACT_ITERATOR(kt_type_byte_iterator, "ByteIterator")
KT_ABSTRACT_ITERATOR(kt_type_short_iterator, "ShortIterator")
KT_ABSTRACT_ITERATOR(kt_type_float_iterator, "FloatIterator")
KT_ABSTRACT_ITERATOR(kt_type_double_iterator, "DoubleIterator")
#undef KT_ABSTRACT_ITERATOR

/* An array's iterator, one class per array kind as Kotlin/Native has: `kotlin.ArrayIterator` for an
   `Array<T>`, `kotlin.IntArrayIterator` (a subclass of `IntIterator`) and kin for the primitive
   arrays, and `kotlin.UIntArray.Iterator` and kin for the unsigned ones, which subclass `Any` and
   implement `Iterator`. The JVM names the signed ones `kotlin.jvm.internal.ArrayIntIterator` and
   so on, from the same superclasses; this runtime is the native one. They share `KWalk` and are
   told apart from every other iterator by being entries of this one table, in `kt_array_kinds`'s
   order. */
#define KT_ARRAY_ITERATOR(package, simple, parent, marker, markers)                                \
    {KT_NAMED(package, simple),                                                                    \
     .instance_size = sizeof(KWalk),                                                               \
     .reference_count = 1,                                                                         \
     .reference_offsets = kt_walk_offsets,                                                         \
     .super = parent,                                                                              \
     .vtable = kt_any_vtable,                                                                      \
     .vtable_length = 3,                                                                           \
     .interfaces = marker,                                                                         \
     .interface_count = markers}
static const KType kt_array_iterators[] = {
    KT_ARRAY_ITERATOR("kotlin.", "ArrayIterator", &kt_type_any, kt_iterator_interfaces, 1),
    KT_ARRAY_ITERATOR("kotlin.", "BooleanArrayIterator", &kt_type_boolean_iterator, NULL, 0),
    KT_ARRAY_ITERATOR("kotlin.", "ByteArrayIterator", &kt_type_byte_iterator, NULL, 0),
    KT_ARRAY_ITERATOR("kotlin.", "CharArrayIterator", &kt_type_char_iterator, NULL, 0),
    KT_ARRAY_ITERATOR("kotlin.", "ShortArrayIterator", &kt_type_short_iterator, NULL, 0),
    KT_ARRAY_ITERATOR("kotlin.", "IntArrayIterator", &kt_type_int_iterator, NULL, 0),
    KT_ARRAY_ITERATOR("kotlin.", "LongArrayIterator", &kt_type_long_iterator, NULL, 0),
    KT_ARRAY_ITERATOR("kotlin.", "FloatArrayIterator", &kt_type_float_iterator, NULL, 0),
    KT_ARRAY_ITERATOR("kotlin.", "DoubleArrayIterator", &kt_type_double_iterator, NULL, 0),
    KT_ARRAY_ITERATOR("kotlin.UByteArray.", "Iterator", &kt_type_any, kt_iterator_interfaces, 1),
    KT_ARRAY_ITERATOR("kotlin.UShortArray.", "Iterator", &kt_type_any, kt_iterator_interfaces, 1),
    KT_ARRAY_ITERATOR("kotlin.UIntArray.", "Iterator", &kt_type_any, kt_iterator_interfaces, 1),
    KT_ARRAY_ITERATOR("kotlin.ULongArray.", "Iterator", &kt_type_any, kt_iterator_interfaces, 1),
};
#undef KT_ARRAY_ITERATOR

/* The array kinds, in `kt_array_iterators`'s order. */
static const KType *const kt_array_kinds[] = {
    &kt_type_array,       &kt_type_boolean_array, &kt_type_byte_array,  &kt_type_char_array,
    &kt_type_short_array, &kt_type_int_array,     &kt_type_long_array,  &kt_type_float_array,
    &kt_type_double_array, &kt_type_ubyte_array,  &kt_type_ushort_array, &kt_type_uint_array,
    &kt_type_ulong_array};

/* The iterator class of an array of kind `array`, which `kt_is_array` has accepted. */
static const KType *kt_array_iterator_type(const KType *array) {
    for (unsigned at = 0; at < sizeof(kt_array_kinds) / sizeof(kt_array_kinds[0]); at++) {
        if (kt_array_kinds[at] == array) {
            return &kt_array_iterators[at];
        }
    }
    KT_FAIL("krusty: an iterator of an array of no known kind\n");
    return NULL;
}

const KType kt_type_chars_iterator = {
    KT_ANONYMOUS("kotlin.collections.CharIterator"),
    .instance_size = sizeof(KWalk),
    .reference_count = 1,
    .reference_offsets = kt_walk_offsets,
    .super = &kt_type_char_iterator,
    .vtable = kt_any_vtable,
    .vtable_length = 3,
    .interfaces = kt_iterator_interfaces,
    .interface_count = sizeof(kt_iterator_interfaces) / sizeof(KType *)};

kt_boolean kt_walk_is(KRef iterator) {
    if (iterator == NULL) {
        return false;
    }
    /* Whether the descriptor is an entry of `kt_array_iterators`, asked of the addresses as
       integers: C orders pointers only within one object. */
    uintptr_t offset = (uintptr_t)iterator->header.type - (uintptr_t)kt_array_iterators;
    return offset < sizeof(kt_array_iterators) || iterator->header.type == &kt_type_chars_iterator;
}

/* Whether a descriptor is one of the thirteen array shapes. */
static kt_boolean kt_is_array(const KType *type) {
    return type == &kt_type_array || type == &kt_type_byte_array || type == &kt_type_short_array
           || type == &kt_type_int_array || type == &kt_type_long_array
           || type == &kt_type_char_array || type == &kt_type_boolean_array
           || type == &kt_type_float_array || type == &kt_type_double_array
           || type == &kt_type_ubyte_array || type == &kt_type_ushort_array
           || type == &kt_type_uint_array || type == &kt_type_ulong_array;
}

static KRef kt_walk_of(const KType *type, KRef over) {
    KWalk *walk = (KWalk *)kt_gc_allocate(type, sizeof(KWalk));
    walk->over = over;
    walk->at = 0;
    walk->byte_at = 0;
    walk->trailing = 0;
    return (KRef)walk;
}

/* Whether the chars iterator has a unit left, and the next one. A `String` is walked from the
   place the iterator keeps, through the same decoder `compareTo` uses; anything else -- a builder,
   text the program wrote -- by index against its length, which is what Kotlin's own
   `CharSequence.iterator()` does and what sees a write made to a builder mid-walk. */
static kt_boolean kt_chars_has_next(const KWalk *walk) {
    if (walk->over->header.type == &kt_type_string) {
        return walk->trailing != 0 || walk->byte_at < walk->over->as.string.byte_length;
    }
    return walk->at < kt_string_length(walk->over);
}

/* The caller has asked `kt_chars_has_next` first. */
static kt_char kt_chars_next(KWalk *walk) {
    kt_int at = walk->at;
    walk->at = at + 1;
    if (walk->over->header.type != &kt_type_string) {
        return kt_string_get(walk->over, at);
    }
    KUnits units = kt_units_of(walk->over);
    units.at = walk->byte_at;
    units.pending = walk->trailing;
    kt_char unit = 0;
    (void)kt_units_next(&units, &unit);
    walk->byte_at = units.at;
    walk->trailing = (kt_char)units.pending;
    return unit;
}

/* One element of an array, boxed by the descriptor the ARRAY carries — the only thing that knows
   how wide the element is and how to read its bits.
   Every descriptor `kt_is_array` accepts is answered here by name and the chain ends in a failure
   rather than in a widest-element read: a descriptor this does not recognise is a routing mistake,
   and reading its element as a `Double` would take eight bytes from an array that may hold one. */
static KRef kt_array_element(KRef array, kt_int at) {
    const KType *type = array->header.type;
    const void *elements = (const void *)((const KArray *)array + 1);
    if (type == &kt_type_array) {
        return ((KRef *)elements)[at];
    }
    if (type == &kt_type_byte_array) {
        return kt_box_byte(((const kt_byte *)elements)[at]);
    }
    if (type == &kt_type_short_array) {
        return kt_box_short(((const kt_short *)elements)[at]);
    }
    if (type == &kt_type_int_array) {
        return kt_box_int(((const kt_int *)elements)[at]);
    }
    if (type == &kt_type_long_array) {
        return kt_box_long(((const kt_long *)elements)[at]);
    }
    if (type == &kt_type_char_array) {
        return kt_box_char(((const kt_char *)elements)[at]);
    }
    if (type == &kt_type_boolean_array) {
        return kt_box_boolean(((const kt_boolean *)elements)[at]);
    }
    if (type == &kt_type_float_array) {
        return kt_box_float(((const kt_float *)elements)[at]);
    }
    if (type == &kt_type_double_array) {
        return kt_box_double(((const kt_double *)elements)[at]);
    }
    /* An unsigned array holds the signed array's bits; only the BOX differs, because it is the box
       that decides whether `toString` reads them as the maximum or as `-1`. */
    if (type == &kt_type_ubyte_array) {
        return kt_box_ubyte(((const kt_byte *)elements)[at]);
    }
    if (type == &kt_type_ushort_array) {
        return kt_box_ushort(((const kt_short *)elements)[at]);
    }
    if (type == &kt_type_uint_array) {
        return kt_box_uint(((const kt_int *)elements)[at]);
    }
    if (type == &kt_type_ulong_array) {
        return kt_box_ulong(((const kt_long *)elements)[at]);
    }
    KT_FAIL("krusty: this is not an array whose elements can be read\n");
    return NULL;
}

/* `xs.toList()` and `xs.reversed()` — a SNAPSHOT of an array's elements as a list. The snapshot is
   the point: Kotlin's own answer is a new list, so writing through the array afterwards leaves it
   as it was, and the corpus checks exactly that.

   The elements are filled one at a time rather than copied, because a List holds references and a
   primitive array does not: each element is boxed on the way in, which is what `IntArray.toList()`
   answering a `List<Int>` means. The destination array is a root across those allocations, in a
   local the conservative scan reads. */
static KRef kt_array_snapshot(KRef array, int reversed) {
    if (array == NULL) {
        kt_null_receiver();
        return NULL;
    }
    kt_int length = ((const KArray *)array)->length;
    KRef elements = kt_array_new(&kt_type_array, length);
    for (kt_int index = 0; index < length; index++) {
        KRef value = kt_array_element(array, reversed ? length - 1 - index : index);
        kt_elements_of(elements)[index] = value;
    }
    return kt_list_of(elements);
}

KRef kt_array_to_list(KRef array) { return kt_array_snapshot(array, 0); }

/* `xs.isEmpty()` / `xs.isNotEmpty()` on an ARRAY. The length is the whole of the question, and it
   is the same question for a reference array and a primitive one. */
kt_boolean kt_array_is_empty(KRef array) {
    if (array == NULL) {
        kt_null_receiver();
        return false;
    }
    return ((const KArray *)array)->length == 0;
}

kt_boolean kt_array_is_not_empty(KRef array) { return !kt_array_is_empty(array); }

/* `xs.toTypedArray()`: a reference `Array<T>` holding what the receiver walks.
   A collection's elements are already references — a primitive one is boxed the moment it enters a
   list — so this is a copy into a fresh array rather than a conversion of each. The SIZE is asked
   first and the array allocated before anything is read, so the walk fills a home that already
   exists. */
KRef kt_iterable_to_typed_array(KRef iterable) {
    KRef list = kt_iterable_to_list(iterable);
    /* A walk that raised answered no list, and its exception is the caller's to see. */
    if (kt_raised()) {
        return NULL;
    }
    kt_int length = kt_list_size(list);
    KRef array = kt_array_new(&kt_type_array, length);
    for (kt_int index = 0; index < length; index++) {
        kt_elements_of(array)[index] = kt_list_get(list, index);
    }
    return array;
}

KRef kt_array_reversed(KRef array) { return kt_array_snapshot(array, 1); }

/* `xs.reversedArray()`: a new ARRAY of the same element type, backwards.

   Unlike `reversed()`, which answers a LIST of boxes, this keeps the elements where they were — in
   an array wearing the receiver's own descriptor, so a primitive array stays primitive. The
   elements are copied as BYTES: the descriptor's stride is what says how wide one is, and copying
   by width is the one answer that serves a reference array and a `DoubleArray` alike. */
KRef kt_array_reversed_array(KRef array) {
    if (array == NULL) {
        kt_null_receiver();
        return NULL;
    }
    const KType *type = array->header.type;
    kt_int length = ((const KArray *)array)->length;
    KRef copy = kt_array_new(type, length);
    size_t stride = (size_t)type->element_size;
    const char *from = (const char *)array + type->instance_size;
    char *to = (char *)copy + type->instance_size;
    for (kt_int index = 0; index < length; index++) {
        memcpy(to + (size_t)(length - 1 - index) * stride, from + (size_t)index * stride, stride);
    }
    return copy;
}

/* An annotation member's array, compared by CONTENT — `Arrays.equals`, which is what Kotlin gives
   an annotation instance's `equals` for an array member and what separates it from a data class's
   (that one compares arrays by identity).

   Each element is compared the way its BOX compares, so a `Float` or `Double` element uses the
   total order: NaN equals itself and the two zeroes are distinct. */
kt_boolean kt_array_content_equals(KRef left, KRef right) {
    if (left == right) {
        return true;
    }
    if (left == NULL || right == NULL) {
        return false;
    }
    kt_int length = ((const KArray *)left)->length;
    if (length != ((const KArray *)right)->length) {
        return false;
    }
    for (kt_int index = 0; index < length; index++) {
        kt_boolean equal = kt_equals(kt_array_element(left, index), kt_array_element(right, index));
        /* An element's `equals` that threw ends the comparison whatever it returned, as it ends
           `kt_list_equals`. */
        if (kt_raised()) {
            return false;
        }
        if (!equal) {
            return false;
        }
    }
    return true;
}

/* `Arrays.hashCode`: 1, then `31 * result + element.hashCode()` per element, a null element
   contributing 0 and a null array answering 0. The arithmetic is on the unsigned ring, because
   Kotlin's `Int` wraps where C's signed overflow is undefined. */
kt_int kt_array_content_hash_code(KRef array) {
    if (array == NULL) {
        return 0;
    }
    kt_int length = ((const KArray *)array)->length;
    uint32_t result = 1;
    for (kt_int index = 0; index < length; index++) {
        KRef element = kt_array_element(array, index);
        uint32_t hash = element == NULL ? 0u : (uint32_t)kt_hash_code(element);
        if (kt_raised()) {
            return 0;
        }
        result = result * 31u + hash;
    }
    return (kt_int)result;
}

/* `Arrays.toString`: `[a, b]`, and the text `null` for a null array. */
KRef kt_array_content_to_string(KRef array) {
    if (array == NULL) {
        return kt_string_utf8("null", 4);
    }
    KRef builder = kt_string_builder_new();
    kt_string_builder_append(builder, kt_string_utf8("[", 1));
    kt_int length = ((const KArray *)array)->length;
    for (kt_int index = 0; index < length; index++) {
        if (index != 0) {
            kt_string_builder_append(builder, kt_string_utf8(", ", 2));
        }
        kt_string_builder_append(builder, kt_array_element(array, index));
        /* The append leaves the builder alone when the element's `toString` throws, but the walk
           has to end there too, or the elements after it are rendered. */
        if (kt_raised()) {
            return NULL;
        }
    }
    kt_string_builder_append(builder, kt_string_utf8("]", 1));
    return kt_to_string(builder);
}

/* Whether a walk reads a `CharSequence` the PROGRAM implements. Such a walk steps the way Kotlin's
   own `CharIterator` does: `hasNext` asks `length`, and `next` is `get(index++)` and nothing more,
   so an index past the end is the program's `get` to answer. Asking `length` again before each
   read would call the program's getter twice per element, which it can observe. */
static kt_boolean kt_walk_reads_program_text(KRef iterator) {
    return iterator->header.type == &kt_type_chars_iterator &&
           ((const KWalk *)iterator)->over->header.type->walk_length != NULL;
}

kt_boolean kt_walk_has_next(KRef iterator) {
    const KWalk *walk = (const KWalk *)iterator;
    if (iterator->header.type == &kt_type_chars_iterator) {
        return kt_chars_has_next(walk);
    }
    return walk->at < kt_length_of(walk->over);
}

/* `next()` on an exhausted array or string iterator, which raises and answers whether it did.

   An array's raises what Kotlin/Native's `ArrayIterator` (and `IntArrayIterator` and kin) raises,
   `NoSuchElementException`, with the JVM's message: `Index 1 out of bounds for length 1` for the
   second `next()` of a one-element array, where Kotlin/Native's is the index alone. A string's is
   Kotlin's `CharSequence.iterator()`, whose `next()` is `get(index++)` with no check of its own, so
   it raises what reading the text past its end raises -- `s[s.length]`'s out-of-bounds exception --
   and never `NoSuchElementException`; and the index moves on before the read raises, so a second
   `next()` asks for the index after. Text the PROGRAM implements is asked its own `get` instead, by
   the caller. */
static kt_boolean kt_walk_exhausted(KRef iterator) {
    if (kt_walk_reads_program_text(iterator) || kt_walk_has_next(iterator)) {
        return false;
    }
    KWalk *walk = (KWalk *)iterator;
    if (iterator->header.type == &kt_type_chars_iterator) {
        kt_int at = walk->at;
        /* On the unsigned ring: a program that catches every failure can ask forever. */
        walk->at = (kt_int)((uint32_t)at + 1u);
        (void)kt_string_get(walk->over, at);
        return true;
    }
    KRef message = kt_string_plus(kt_string_utf8("Index ", 6), kt_box_int(walk->at));
    message = kt_string_plus(message, kt_string_utf8(" out of bounds for length ", 26));
    message = kt_string_plus(message, kt_box_int(kt_length_of(walk->over)));
    kt_throw(kt_throwable_new(&kt_type_no_such_element_exception, message));
    return true;
}

/* The element as the 64 bits the narrow iterator protocol carries. A REFERENCE array's element is
   not a number and must never arrive here: an `Array<T>`'s iterator has the static type
   `Iterator<T>`, which routes to the general dispatch instead, so reaching this with one means the
   routing above went wrong rather than that a pointer should be returned as an integer. */
kt_long kt_walk_next_long(KRef iterator) {
    KWalk *walk = (KWalk *)iterator;
    if (kt_walk_exhausted(iterator)) {
        return 0;
    }
    if (iterator->header.type == &kt_type_chars_iterator) {
        return kt_chars_next(walk);
    }
    kt_int at = walk->at;
    walk->at = at + 1;
    const KType *type = walk->over->header.type;
    const void *elements = (const void *)((const KArray *)walk->over + 1);
    if (type == &kt_type_byte_array) {
        return ((const kt_byte *)elements)[at];
    }
    if (type == &kt_type_short_array) {
        return ((const kt_short *)elements)[at];
    }
    if (type == &kt_type_int_array) {
        return ((const kt_int *)elements)[at];
    }
    if (type == &kt_type_long_array) {
        return ((const kt_long *)elements)[at];
    }
    if (type == &kt_type_char_array) {
        return ((const kt_char *)elements)[at];
    }
    if (type == &kt_type_boolean_array) {
        return ((const kt_boolean *)elements)[at];
    }
    /* The unsigned widths, read through their own C types so the 64 bits this protocol carries hold
       the VALUE and not its sign extension: a `UByteArray`'s `255u` arrives as 255, where a signed
       read of the same byte would arrive as -1 and render that way anywhere the consumer widens
       before it narrows again. */
    if (type == &kt_type_ubyte_array) {
        return ((const uint8_t *)elements)[at];
    }
    if (type == &kt_type_ushort_array) {
        return ((const uint16_t *)elements)[at];
    }
    if (type == &kt_type_uint_array) {
        return ((const uint32_t *)elements)[at];
    }
    if (type == &kt_type_ulong_array) {
        return (kt_long)((const uint64_t *)elements)[at];
    }
    KT_FAIL("krusty: this iterator does not answer a number\n");
    return 0;
}

/* ---- withIndex ------------------------------------------------------------------------------ */

/* `IndexedValue(index, value)`, Kotlin's own data class. Only `value` is a reference; the index is
   an `Int` and the collector is told so. */
typedef struct KIndexedValue {
    KObjectHeader header;
    KRef value;
    kt_int index;
} KIndexedValue;

static const uint32_t kt_indexed_value_offsets[] = {offsetof(KIndexedValue, value)};

static kt_boolean kt_indexed_value_equals(KRef self, KRef other);
static kt_int kt_indexed_value_hash_code(KRef self);
static KRef kt_indexed_value_to_string(KRef self);

static const kt_fn kt_indexed_value_vtable[] = {(kt_fn)kt_indexed_value_equals,
                                                (kt_fn)kt_indexed_value_hash_code,
                                                (kt_fn)kt_indexed_value_to_string};

const KType kt_type_indexed_value = {
    KT_NAMED("kotlin.collections.", "IndexedValue"),
    .instance_size = sizeof(KIndexedValue),
    .reference_count = 1,
    .reference_offsets = kt_indexed_value_offsets,
    .super = &kt_type_any,
    .vtable = kt_indexed_value_vtable,
    .vtable_length = 3};

KRef kt_indexed_value(kt_int index, KRef value) {
    /* `value` stays in the parameter across the allocation: it is its root. */
    KIndexedValue *indexed =
        (KIndexedValue *)kt_gc_allocate(&kt_type_indexed_value, sizeof(KIndexedValue));
    indexed->index = index;
    indexed->value = value;
    return (KRef)indexed;
}

kt_int kt_indexed_value_index(KRef self) {
    if (self == NULL) {
        kt_null_receiver();
        return 0;
    }
    return ((const KIndexedValue *)self)->index;
}

KRef kt_indexed_value_value(KRef self) {
    if (self == NULL) {
        kt_null_receiver();
        return NULL;
    }
    return ((const KIndexedValue *)self)->value;
}

/* A data class: equal by its components, which is what a program comparing two of them means. */
static kt_boolean kt_indexed_value_equals(KRef self, KRef other) {
    if (other == NULL || other->header.type != &kt_type_indexed_value) {
        return 0;
    }
    const KIndexedValue *mine = (const KIndexedValue *)self;
    const KIndexedValue *theirs = (const KIndexedValue *)other;
    if (mine->index != theirs->index) {
        return 0;
    }
    /* The value's `equals` is the program's and may throw; what it returned then is no answer. */
    kt_boolean equal = kt_equals(mine->value, theirs->value);
    return !kt_raised() && equal;
}

static kt_int kt_indexed_value_hash_code(KRef self) {
    const KIndexedValue *indexed = (const KIndexedValue *)self;
    /* The value's `hashCode` may throw, and its answer is then no hash to fold in. */
    kt_int value_hash = kt_hash_code(indexed->value);
    if (kt_raised()) {
        return 0;
    }
    /* On the unsigned ring, as `kt_list_hash_code` computes: Kotlin's `Int` wraps, and C's signed
       overflow is undefined rather than a wrap. */
    return (kt_int)((uint32_t)indexed->index * 31u + (uint32_t)value_hash);
}

static KRef kt_indexed_value_to_string(KRef self) {
    const KIndexedValue *indexed = (const KIndexedValue *)self;
    KRef text = kt_string_utf8("IndexedValue(index=", 19);
    text = kt_string_plus(text, kt_to_string(kt_box_int(indexed->index)));
    text = kt_string_plus(text, kt_string_utf8(", value=", 8));
    KRef rendered = kt_to_string(indexed->value);
    /* A value whose `toString` threw has no text, and `kt_string_plus` would render its NULL answer
       as `null`; the rendering ends with the exception instead. */
    if (kt_raised()) {
        return NULL;
    }
    text = kt_string_plus(text, rendered);
    return kt_string_plus(text, kt_string_utf8(")", 1));
}

/* What `withIndex()` answers: the source iterable, kept until somebody asks it for an iterator.
   Lazy, because Kotlin's is and because the loop that consumes it may stop early. */
typedef struct KWithIndex {
    KObjectHeader header;
    KRef source;
} KWithIndex;

static const uint32_t kt_with_index_offsets[] = {offsetof(KWithIndex, source)};

const KType kt_type_with_index = {
    KT_NAMED("kotlin.collections.", "IndexingIterable"),
    .instance_size = sizeof(KWithIndex),
    .reference_count = 1,
    .reference_offsets = kt_with_index_offsets,
    .super = &kt_type_any,
    .vtable = kt_any_vtable,
    .vtable_length = 3,
    .interfaces = kt_iterable_bases,
    .interface_count = sizeof(kt_iterable_bases) / sizeof(KType *)};

/* What `asSequence()` answers: the source iterable, kept until somebody asks it for an iterator.

   The same shape as `withIndex()` above and lazy for the same reason, but its own type — because
   what separates a `Sequence` from an `Iterable` here is which members may be asked of it. Every
   walk this runtime has is EAGER, and an eager `map` on a sequence is not Kotlin's: the transform
   would run for every element where Kotlin runs it per element consumed, which a side effect sees
   and an endless sequence never survives. So a sequence is offered only the members whose answer is
   the same either way — its iterator, and the lazy `withIndex` — and the rest decline at the call
   site, where the type the program named is still in sight.

   `equals` and `hashCode` are IDENTITY, which is what Kotlin answers: `Sequence` declares neither,
   so two sequences over the same elements are different objects and stay that way. */
typedef struct KSequence {
    KObjectHeader header;
    KRef source;
} KSequence;

static const uint32_t kt_sequence_offsets[] = {offsetof(KSequence, source)};

const KType kt_type_sequence = {
    KT_ANONYMOUS("kotlin.sequences.Sequence"),
    .instance_size = sizeof(KSequence),
    .reference_count = 1,
    .reference_offsets = kt_sequence_offsets,
    .super = &kt_type_any,
    .vtable = kt_any_vtable,
    .vtable_length = 3};

KRef kt_sequence_of(KRef source) {
    KSequence *wrapper = (KSequence *)kt_gc_allocate(&kt_type_sequence, sizeof(KSequence));
    wrapper->source = source;
    return (KRef)wrapper;
}

kt_boolean kt_is_sequence(KRef value) {
    return value != NULL && value->header.type == &kt_type_sequence;
}

/* The iterator it hands out: the source's own, plus the count. */
typedef struct KIndexingIterator {
    KObjectHeader header;
    KRef source;
    /* The next index, on the unsigned ring; see `kt_counter_overflowed`. */
    uint32_t at;
} KIndexingIterator;

static const uint32_t kt_indexing_iterator_offsets[] = {offsetof(KIndexingIterator, source)};

const KType kt_type_indexing_iterator = {
    KT_NAMED("kotlin.collections.", "IndexingIterator"),
    .instance_size = sizeof(KIndexingIterator),
    .reference_count = 1,
    .reference_offsets = kt_indexing_iterator_offsets,
    .super = &kt_type_any,
    .vtable = kt_any_vtable,
    .vtable_length = 3,
    .interfaces = kt_iterator_interfaces,
    .interface_count = sizeof(kt_iterator_interfaces) / sizeof(KType *)};

KRef kt_iterable_with_index(KRef iterable) {
    KWithIndex *wrapper = (KWithIndex *)kt_gc_allocate(&kt_type_with_index, sizeof(KWithIndex));
    wrapper->source = iterable;
    return (KRef)wrapper;
}

KRef kt_iterable_iterator(KRef iterable) {
    /* Either list shape: the one list iterator serves both, because its cursor is an index and the
       bound it compares against is `kt_list_size`, which both answer. */
    if (iterable != NULL
        && (iterable->header.type == &kt_type_list || kt_is_mutable_list(iterable))) {
        return kt_list_iterator(iterable);
    }
    if (iterable != NULL && kt_is_array(iterable->header.type)) {
        return kt_walk_of(kt_array_iterator_type(iterable->header.type), iterable);
    }
    /* Either text shape: the chars iterator reads its element through `kt_string_get` and its
       bound through `kt_string_length`, and both answer for a string and for a builder. */
    if (iterable != NULL
        && (iterable->header.type == &kt_type_string || kt_is_string_builder(iterable))) {
        return kt_walk_of(&kt_type_chars_iterator, iterable);
    }
    /* A SEQUENCE is walked as its source: that is the whole of what the wrapper holds, and asking
       it for an iterator is the one member Kotlin's `Sequence` declares. */
    if (kt_is_sequence(iterable)) {
        return kt_iterable_iterator(((const KSequence *)iterable)->source);
    }
    if (iterable != NULL && iterable->header.type == &kt_type_with_index) {
        KRef source = kt_iterable_iterator(((const KWithIndex *)iterable)->source);
        KIndexingIterator *counting = (KIndexingIterator *)kt_gc_allocate(
            &kt_type_indexing_iterator, sizeof(KIndexingIterator));
        counting->source = source;
        counting->at = kt_walk_counter_origin;
        return (KRef)counting;
    }
    /* A class of the PROGRAM that implements `kotlin.collections.Iterable`: its own `iterator()`,
       at the slot its descriptor records. The maps, sets and views of `krusty_maps.c` answer here
       too, through the walk defined beside them, and a map walks its entries, which is what
       Kotlin's `Map.iterator()` extension answers and what `for ((k, v) in m)` destructures. Last,
       so that nothing this runtime makes is reached through a dispatch when its own shape already
       answered. */
    if (iterable != NULL && iterable->header.type->walk_iterator != NULL) {
        return iterable->header.type->walk_iterator(iterable);
    }
    /* Text the PROGRAM wrote is walked the way this runtime's own text is: by index, against the
       length. `kt_string_length` and `kt_string_get` reach its own members, so the chars iterator
       needs no case of its own. */
    if (iterable != NULL && iterable->header.type->walk_length != NULL) {
        return kt_walk_of(&kt_type_chars_iterator, iterable);
    }
    return kt_range_iterator(iterable);
}

kt_boolean kt_iterator_has_next(KRef iterator) {
    if (kt_is_list_iterator(iterator)) {
        return kt_list_iterator_has_next(iterator);
    }
    if (kt_walk_is(iterator)) {
        return kt_walk_has_next(iterator);
    }
    if (iterator != NULL && iterator->header.type == &kt_type_indexing_iterator) {
        return kt_iterator_has_next(((const KIndexingIterator *)iterator)->source);
    }
    if (iterator != NULL && iterator->header.type->walk_has_next != NULL) {
        return iterator->header.type->walk_has_next(iterator);
    }
    return kt_range_iterator_has_next(iterator);
}

/* Whether a walk has another element. A program's `iterator()` and `hasNext()` are the program's
   code and may throw, and a throw comes back with a placeholder — a NULL or a stray object for an
   iterator, a `true` or a `false` for `hasNext` — that no walk may act on: a `true` would go on to
   `next` and into the program again, a `false` would end the walk as if it were complete, and
   either would let a later raise overwrite the exception in flight. So this answers false once an
   exception is pending, whether it came from the `iterator()` that made `iterator` or from this
   `hasNext`, and it never asks a placeholder iterator anything. A walk that stops on false asks
   `kt_raised` before it answers anything of its own. */
static kt_boolean kt_more(KRef iterator) {
    if (kt_raised()) {
        return false;
    }
    kt_boolean more = kt_iterator_has_next(iterator);
    return !kt_raised() && more;
}

/* Call a one-argument function value. The second place the runtime calls back into emitted code,
   through the same slot `kt_lazy_value` uses; see `KT_SLOT_INVOKE`. */
static KRef kt_invoke_one(KRef function, KRef argument) {
    if (function == NULL || function->header.type->vtable == NULL ||
        function->header.type->vtable_length <= KT_SLOT_INVOKE) {
        KT_FAIL("krusty: a function value was expected here\n");
    }
    return ((KRef(*)(KRef, KRef))function->header.type->vtable[KT_SLOT_INVOKE])(function, argument);
}

/* How many elements an iterable will yield. Only the two this runtime has are askable, and the
   descriptor says which — a range's count is its bounds, a list's is its array's length.

   `map` needs this because it answers a LIST, and a list is an array with a header: there is one
   allocation, sized once, rather than a buffer that grows. It asks only for a receiver its
   transform cannot change; a mutable one is walked by its iterator (see `kt_iterable_map`). */
static kt_int kt_iterable_size(KRef iterable) {
    if (iterable == NULL) {
        kt_null_receiver();
        return 0;
    }
    if (iterable->header.type == &kt_type_list || kt_is_mutable_list(iterable)) {
        return kt_list_size(iterable);
    }
    kt_int held = kt_map_collection_size(iterable);
    if (held >= 0) {
        return held;
    }
    if (kt_is_array(iterable->header.type)) {
        return kt_length_of(iterable);
    }
    /* A `String` never changes, so its length is its walk's. A builder's walk asks the CURRENT
       length before every step, as Kotlin's `CharSequence.iterator()` does, and a transform that
       writes to the builder it maps changes that; it has no size to give ahead of the walk. */
    if (iterable->header.type == &kt_type_string) {
        return kt_string_length(iterable);
    }
    if (!kt_is_range(iterable)) {
        /* A collection of the PROGRAM's, which this side reaches only through its iterator: its
           count is not a question the descriptor answers, and walking it to find out would walk
           it twice. -1 says so, and the one caller sizes its result as it goes instead. */
        return -1;
    }
    /* How many elements the WALK yields, which is not `last - first + 1` unless the step is 1 and
       the walk ascends: a descending `3 downTo 1` has `first` above `last`, and `1..9 step 3`
       yields 3 elements, not 7.

       `kt_range_empty` already answers emptiness for either direction and at the bounds' own
       signedness, so the count below never divides for a walk that yields nothing.

       The span is taken on the RING, as two's-complement subtraction, for the reason
       `kt_range_last_element` states about forming a distance: `Long.MIN_VALUE..Long.MAX_VALUE`
       spans more than a `kt_long` holds, and the signed subtraction wraps. At 64 bits unsigned it
       is exact -- and exact for an unsigned range's bounds above 2^63 by the same token. A span
       that large cannot be collected anyway, which the cap below still says. `last` is already the
       last element REACHED, so the span divides by the step exactly. */
    const KRange *bounds = (const KRange *)iterable;
    if (kt_range_empty(bounds)) {
        return 0;
    }
    kt_long step = bounds->step;
    uint64_t span = step > 0 ? (uint64_t)bounds->last - (uint64_t)bounds->first
                             : (uint64_t)bounds->first - (uint64_t)bounds->last;
    uint64_t magnitude = step > 0 ? (uint64_t)step : (uint64_t)0 - (uint64_t)step;
    /* The count is the number of steps plus one, and the cap is tested on the STEPS: the full
       64-bit span with a step of one is 2^64 - 1 steps, and adding the one wraps the count to zero,
       which would pass the cap and collect the whole range as an empty list. */
    uint64_t steps = span / magnitude;
    if (steps >= (uint64_t)INT32_MAX) {
        KT_FAIL("krusty: a range too long to collect\n");
    }
    return (kt_int)(steps + 1u);
}

/* `xs.map { … }`: one new list, the transform applied to each element in order.

   The result list is built BEFORE the loop so that it, and through it the array, is a root across
   every call the loop makes — each of which may collect, and each of which may allocate whatever
   the transform returns. Elements already written are traced through the array like any other
   reference, so there is nothing to defer and no barrier to write. */
static KRef kt_frozen(KRef growing, kt_boolean reversed);

KRef kt_iterable_map(KRef iterable, KRef transform) {
    kt_int size = kt_iterable_size(iterable);
    if (kt_raised()) {
        return NULL;
    }
    if (size < 0 || kt_is_mutable_list(iterable) || kt_is_set(iterable) || kt_is_map(iterable)) {
        /* A receiver whose count is not known without walking it (see `kt_iterable_size`), or one
           the transform can change while it is walked. Kotlin's `map` is a `for` loop over the
           receiver's iterator, so it asks `hasNext()` before every element and the iterator
           notices a change: a transform that appends to a one-element list makes the walk go on to
           a `next()` that raises `ConcurrentModificationException`, where a walk of the size
           taken first would stop after one element with nothing raised. The result grows rather
           than being sized once, which costs a copy or two and keeps the walk single — and a
           walk's side effects are what a program can see. */
        KRef growing = kt_mutable_list_new();
        KRef walk = kt_iterable_iterator(iterable);
        while (kt_more(walk)) {
            KRef element = kt_iterator_next(walk);
            if (kt_raised()) {
                return NULL;
            }
            KRef mapped = kt_invoke_one(transform, element);
            if (kt_raised()) {
                return NULL;
            }
            kt_mutable_list_add(growing, mapped);
        }
        if (kt_raised()) {
            return NULL;
        }
        return kt_frozen(growing, 0);
    }
    KRef elements = kt_array_new(&kt_type_array, size);
    KRef result = kt_list_of(elements);
    KRef iterator = kt_iterable_iterator(iterable);
    for (kt_int i = 0; i < size; i++) {
        KRef element = kt_iterator_next(iterator);
        if (kt_raised()) {
            return NULL;
        }
        KRef mapped = kt_invoke_one(transform, element);
        if (kt_raised()) {
            return NULL;
        }
        kt_elements_of(elements)[i] = mapped;
    }
    return result;
}

KRef kt_iterable_join_to_string(KRef iterable) {
    KRef separator = kt_string_utf8(", ", 2);
    KRef joined = kt_string_utf8("", 0);
    KRef iterator = kt_iterable_iterator(iterable);
    kt_boolean first = 1;
    while (kt_more(iterator)) {
        if (!first) {
            joined = kt_string_plus(joined, separator);
        }
        first = 0;
        KRef element = kt_iterator_next(iterator);
        if (kt_raised()) {
            return NULL;
        }
        /* `kt_to_string` and not the element itself: `joinToString` renders each element the way
           `"$element"` would, through whatever `toString` the element's own type answers with. A
           `toString` that threw ends the join where it threw, as Kotlin's loop does: its answer is
           no text to append, and no later element is rendered. */
        KRef rendered = kt_to_string(element);
        if (kt_raised()) {
            return NULL;
        }
        joined = kt_string_plus(joined, rendered);
    }
    if (kt_raised()) {
        return NULL;
    }
    return joined;
}

/* `xs.forEach { … }`: the same walk with nothing kept, and so nothing to size. */
void kt_iterable_for_each(KRef iterable, KRef action) {
    KRef iterator = kt_iterable_iterator(iterable);
    while (kt_more(iterator)) {
        KRef element = kt_iterator_next(iterator);
        if (kt_raised()) {
            return;
        }
        (void)kt_invoke_one(action, element);
        if (kt_raised()) {
            return;
        }
    }
}

/* A function value of TWO parameters, called the way `kt_invoke_one` calls one of one. */
static KRef kt_invoke_two(KRef function, KRef first, KRef second) {
    if (function == NULL || function->header.type->vtable == NULL ||
        function->header.type->vtable_length <= KT_SLOT_INVOKE) {
        KT_FAIL("krusty: a function value was expected here\n");
    }
    return ((KRef(*)(KRef, KRef, KRef))function->header.type->vtable[KT_SLOT_INVOKE])(function,
                                                                                      first,
                                                                                      second);
}

/* `list.sortWith(comparator)` / `xs.sortedWith(comparator)`. The comparator is an ordinary
   function value of two arguments — a `Comparator` this runtime makes is a `Function2`, because
   nothing but its one member is ever asked of it — so the comparison goes through the same invoke
   slot every function value declares, and its answer arrives BOXED.

   An INSERTION sort, which is stable, and stability is observable: Kotlin's `sortWith` promises it,
   so two elements the comparator calls equal keep the order they were in. It is quadratic, and the
   corpus's lists are small; a merge sort would need a scratch buffer this has no reason to allocate
   yet. The comparison can collect — it is emitted code — and nothing here holds a raw element
   pointer across one: each step re-reads through the list. */
kt_int kt_comparator_compare(KRef comparator, KRef left, KRef right) {
    KRef answer = kt_invoke_two(comparator, left, right);
    /* A comparator that THREW answered nothing too, and that is the program's exception, which its
       caller must see; the zero is no ordering and the sort below stops on it. */
    if (kt_raised()) {
        return 0;
    }
    if (answer == NULL) {
        KT_FAIL("krusty: a comparator answered nothing\n");
    }
    return kt_unbox_int(answer);
}

void kt_list_sort_with(KRef list, KRef comparator) {
    kt_int size = kt_list_size(list);
    for (kt_int at = 1; at < size; at++) {
        kt_int hole = at;
        while (hole > 0) {
            KRef previous = kt_list_get(list, hole - 1);
            KRef current = kt_list_get(list, hole);
            kt_int order = kt_comparator_compare(comparator, previous, current);
            if (kt_raised()) {
                return;
            }
            if (order <= 0) {
                break;
            }
            (void)kt_mutable_list_set(list, hole - 1, current);
            (void)kt_mutable_list_set(list, hole, previous);
            hole--;
        }
    }
}

KRef kt_invoke_three(KRef function, KRef first, KRef second, KRef third) {
    if (function == NULL || function->header.type->vtable == NULL ||
        function->header.type->vtable_length <= KT_SLOT_INVOKE) {
        KT_FAIL("krusty: a function value was expected here\n");
    }
    return ((KRef(*)(KRef, KRef, KRef, KRef))function->header.type->vtable[KT_SLOT_INVOKE])(
        function, first, second, third);
}

/* Whether a predicate answered true for an element. The answer arrives BOXED, because a function
   value's `invoke` hands back a reference whatever its declared return type is.

   A predicate that threw answered NULL, which is no Boolean to unbox: this answers false for it,
   and every caller asks `kt_raised` before it reads anything into the answer. */
static kt_boolean kt_holds(KRef predicate, KRef element) {
    KRef answer = kt_invoke_one(predicate, element);
    if (kt_raised()) {
        return 0;
    }
    return kt_unbox_boolean(answer);
}

/* The next element of a walk and whether the predicate holds for it, or false when either raised --
   which `kt_raised` then tells the caller. The shared step of every walk below that asks a
   predicate. */
static kt_boolean kt_next_holds(KRef iterator, KRef predicate, KRef *element) {
    *element = kt_iterator_next(iterator);
    if (kt_raised()) {
        return 0;
    }
    return kt_holds(predicate, *element);
}

/* `xs.any { … }`, `xs.all { … }` and `xs.none { … }` — one walk, three readings of it. Each stops
   at the first element that settles the question, which is Kotlin's own promise and is observable
   through a predicate with a side effect. */
kt_boolean kt_iterable_any(KRef iterable, KRef predicate) {
    KRef iterator = kt_iterable_iterator(iterable);
    while (kt_more(iterator)) {
        KRef element = NULL;
        if (kt_next_holds(iterator, predicate, &element)) {
            return 1;
        }
        if (kt_raised()) {
            return 0;
        }
    }
    return 0;
}

kt_boolean kt_iterable_all(KRef iterable, KRef predicate) {
    KRef iterator = kt_iterable_iterator(iterable);
    while (kt_more(iterator)) {
        KRef element = NULL;
        if (!kt_next_holds(iterator, predicate, &element)) {
            return 0;
        }
    }
    return !kt_raised();
}

/* `none` is `any` negated, but not its placeholder: after a raise `any` answers false, which
   negated would be a `true` nobody computed. */
kt_boolean kt_iterable_none(KRef iterable, KRef predicate) {
    kt_boolean any = kt_iterable_any(iterable, predicate);
    return !kt_raised() && !any;
}

/* `xs.any()` and `xs.none()` with no predicate: whether the walk yields anything at all. */
kt_boolean kt_iterable_is_not_empty(KRef iterable) {
    return kt_more(kt_iterable_iterator(iterable));
}

kt_boolean kt_iterable_is_empty(KRef iterable) {
    kt_boolean not_empty = kt_iterable_is_not_empty(iterable);
    return !kt_raised() && !not_empty;
}

/* `xs.count()` walks rather than reading a size: `count` is declared over `Iterable`, and the
   walk is the only thing every iterable has. */
kt_int kt_iterable_count(KRef iterable) {
    KRef iterator = kt_iterable_iterator(iterable);
    uint32_t counted = kt_walk_counter_origin;
    while (kt_more(iterator)) {
        (void)kt_iterator_next(iterator);
        if (kt_raised() || kt_counter_overflowed(++counted, KT_COUNTS_ELEMENTS)) {
            return 0;
        }
    }
    return kt_raised() ? 0 : (kt_int)counted;
}

kt_int kt_iterable_count_matching(KRef iterable, KRef predicate) {
    KRef iterator = kt_iterable_iterator(iterable);
    uint32_t counted = kt_walk_counter_origin;
    while (kt_more(iterator)) {
        KRef element = NULL;
        kt_boolean holds = kt_next_holds(iterator, predicate, &element);
        if (kt_raised() || (holds && kt_counter_overflowed(++counted, KT_COUNTS_ELEMENTS))) {
            return 0;
        }
    }
    return kt_raised() ? 0 : (kt_int)counted;
}

/* Every element a walk yields, collected without asking the iterable for a SIZE.

   Not every iterable has one to give: `withIndex()` answers a lazy object that only knows how to
   walk, and a filter's answer is shorter than its source by definition. The buffer is the growable
   list the runtime already has, which doubles; the answer is a READ-ONLY list over an array of
   exactly the right length, because a read-only list IS its array and nothing may see a spare
   slot. */
static KRef kt_frozen(KRef growing, kt_boolean reversed) {
    kt_int size = kt_list_size(growing);
    KRef elements = kt_array_new(&kt_type_array, size);
    /* Built before the copy so the array is a root through it; `growing` is one in this frame. */
    KRef result = kt_list_of(elements);
    for (kt_int at = 0; at < size; at++) {
        kt_elements_of(elements)[reversed ? size - 1 - at : at] = kt_list_get(growing, at);
    }
    return result;
}

/* `xs.filter { … }` and `xs.filterNot { … }`.

   The predicate is asked once per element, which counting first and filling second would not
   manage — a predicate may have a side effect, and Kotlin asks it once.

   `keep` is what the predicate must answer for an element to survive, so one walk serves both
   names. */
static KRef kt_iterable_filtered(KRef iterable, KRef predicate, kt_boolean keep) {
    KRef growing = kt_mutable_list_new();
    KRef iterator = kt_iterable_iterator(iterable);
    while (kt_more(iterator)) {
        KRef element = NULL;
        kt_boolean holds = kt_next_holds(iterator, predicate, &element);
        if (kt_raised()) {
            return NULL;
        }
        if (holds == keep) {
            kt_mutable_list_add(growing, element);
        }
    }
    if (kt_raised()) {
        return NULL;
    }
    return kt_frozen(growing, 0);
}

KRef kt_iterable_filter(KRef iterable, KRef predicate) {
    return kt_iterable_filtered(iterable, predicate, 1);
}

KRef kt_iterable_filter_not(KRef iterable, KRef predicate) {
    return kt_iterable_filtered(iterable, predicate, 0);
}

/* `xs.first { … }` and `xs.firstOrNull { … }`. Kotlin raises `NoSuchElementException` when nothing
   matches, with its own wording; the raise is followed by a RETURN, because `kt_throw` records the
   exception for the call site and comes back. */
KRef kt_iterable_first_or_null(KRef iterable, KRef predicate) {
    KRef iterator = kt_iterable_iterator(iterable);
    while (kt_more(iterator)) {
        KRef element = NULL;
        if (kt_next_holds(iterator, predicate, &element)) {
            return element;
        }
        if (kt_raised()) {
            return NULL;
        }
    }
    return NULL;
}

KRef kt_iterable_first_matching(KRef iterable, KRef predicate) {
    KRef iterator = kt_iterable_iterator(iterable);
    while (kt_more(iterator)) {
        KRef element = NULL;
        if (kt_next_holds(iterator, predicate, &element)) {
            return element;
        }
        /* The predicate's own exception, which must not be replaced by the one below. */
        if (kt_raised()) {
            return NULL;
        }
    }
    /* Nor the exception an `iterator()` or `hasNext()` of the program's threw. */
    if (kt_raised()) {
        return NULL;
    }
    kt_throw(kt_throwable_new(
        &kt_type_no_such_element_exception,
        kt_string_utf8("Collection contains no element matching the predicate.", 54)));
    return NULL;
}

/* `xs.last { … }`: the LAST match, so the whole walk runs. */
KRef kt_iterable_last_matching(KRef iterable, KRef predicate) {
    KRef iterator = kt_iterable_iterator(iterable);
    KRef found = NULL;
    kt_boolean any = 0;
    while (kt_more(iterator)) {
        KRef element = NULL;
        kt_boolean holds = kt_next_holds(iterator, predicate, &element);
        if (kt_raised()) {
            return NULL;
        }
        if (holds) {
            found = element;
            any = 1;
        }
    }
    if (kt_raised()) {
        return NULL;
    }
    if (!any) {
        kt_throw(kt_throwable_new(
            &kt_type_no_such_element_exception,
            kt_string_utf8("Collection contains no element matching the predicate.", 54)));
        return NULL;
    }
    return found;
}

/* `xs.fold(initial) { acc, e -> … }`: the accumulator threaded through the walk. */
KRef kt_iterable_fold(KRef iterable, KRef initial, KRef operation) {
    KRef accumulator = initial;
    KRef iterator = kt_iterable_iterator(iterable);
    while (kt_more(iterator)) {
        KRef element = kt_iterator_next(iterator);
        if (kt_raised()) {
            return NULL;
        }
        accumulator = kt_invoke_two(operation, accumulator, element);
        if (kt_raised()) {
            return NULL;
        }
    }
    return kt_raised() ? NULL : accumulator;
}

/* `xs.forEachIndexed { i, e -> … }`. The index is BOXED on the way in, because a function value
   takes references; the lambda's own prologue unboxes it. */
void kt_iterable_for_each_indexed(KRef iterable, KRef action) {
    KRef iterator = kt_iterable_iterator(iterable);
    uint32_t index = kt_walk_counter_origin;
    while (kt_more(iterator)) {
        KRef element = kt_iterator_next(iterator);
        if (kt_raised() || kt_counter_overflowed(index, KT_COUNTS_INDICES)) {
            return;
        }
        (void)kt_invoke_two(action, kt_box_int((kt_int)index), element);
        if (kt_raised()) {
            return;
        }
        index++;
    }
}

/* `xs.toList()` and `xs.reversed()` over an ITERABLE: a snapshot of its elements, in order or
   backwards. The array version of both is `kt_array_to_list`/`kt_array_reversed`; this one walks,
   which is what a range and a lazy `withIndex()` need. */
static KRef kt_iterable_snapshot(KRef iterable, kt_boolean reversed) {
    KRef growing = kt_mutable_list_new();
    KRef iterator = kt_iterable_iterator(iterable);
    while (kt_more(iterator)) {
        KRef element = kt_iterator_next(iterator);
        if (kt_raised()) {
            return NULL;
        }
        kt_mutable_list_add(growing, element);
    }
    if (kt_raised()) {
        return NULL;
    }
    return kt_frozen(growing, reversed);
}

KRef kt_iterable_to_list(KRef iterable) { return kt_iterable_snapshot(iterable, 0); }

KRef kt_iterable_reversed(KRef iterable) { return kt_iterable_snapshot(iterable, 1); }

/* `xs.sortedWith(comparator)`: a new list, the receiver untouched. Sorted while the list is still
   the GROWING shape `kt_mutable_list_set` writes through, and frozen afterwards — a frozen list has
   no `size` beside its elements and is not the same object to write into. */
KRef kt_iterable_sorted_with(KRef iterable, KRef comparator) {
    KRef growing = kt_mutable_list_new();
    KRef iterator = kt_iterable_iterator(iterable);
    while (kt_more(iterator)) {
        KRef element = kt_iterator_next(iterator);
        if (kt_raised()) {
            return NULL;
        }
        (void)kt_mutable_list_add(growing, element);
    }
    if (kt_raised()) {
        return NULL;
    }
    kt_list_sort_with(growing, comparator);
    if (kt_raised()) {
        return NULL;
    }
    return kt_frozen(growing, 0);
}

/* `value in xs` and `xs.indexOf(value)` over an ITERABLE. Elements are compared with `equals`, as
   Kotlin's own are — the list form already does, and a range's is the same question asked of the
   numbers it yields. */
kt_int kt_iterable_index_of(KRef iterable, KRef value) {
    KRef iterator = kt_iterable_iterator(iterable);
    uint32_t at = kt_walk_counter_origin;
    while (kt_more(iterator)) {
        KRef element = kt_iterator_next(iterator);
        if (kt_raised() || kt_counter_overflowed(at, KT_COUNTS_INDICES)) {
            return -1;
        }
        /* The ARGUMENT's `equals`, as Kotlin's `element == item` asks. As in `kt_list_index_of`, an
           `equals` that threw ends the search with no index. */
        kt_boolean equal = kt_equals(value, element);
        if (kt_raised()) {
            return -1;
        }
        if (equal) {
            return (kt_int)at;
        }
        at++;
    }
    return -1;
}

/* Kotlin's `Iterable.contains` asks a `Collection` its own `contains`, which for a set or a map's
   view is a hash lookup rather than this walk. A map is no `Iterable`, so it is not asked. */
kt_boolean kt_iterable_contains(KRef iterable, KRef value) {
    if (!kt_is_map(iterable) && kt_map_collection_size(iterable) >= 0) {
        return kt_set_contains(iterable, value);
    }
    return kt_iterable_index_of(iterable, value) >= 0;
}

/* `xs + x` and `xs + ys`: a NEW read-only list, never a change to the receiver — that is what
   separates `plus` from `plusAssign`, and Kotlin's contract is that a `List` cannot be changed at
   all. Which of the two a call means is the CALLER's answer, read from the physical parameter the
   same way `plusAssign` reads it: after substitution an element of type `List<T>` and a collection
   of them look alike, and only the declaration tells them apart. */
static KRef kt_iterable_walked_into(KRef iterable, KRef growing) {
    KRef iterator = kt_iterable_iterator(iterable);
    while (kt_more(iterator)) {
        KRef element = kt_iterator_next(iterator);
        if (kt_raised()) {
            return growing;
        }
        kt_mutable_list_add(growing, element);
    }
    return growing;
}

KRef kt_iterable_plus_element(KRef iterable, KRef element) {
    KRef growing = kt_iterable_walked_into(iterable, kt_mutable_list_new());
    if (kt_raised()) {
        return NULL;
    }
    kt_mutable_list_add(growing, element);
    return kt_frozen(growing, 0);
}

KRef kt_iterable_plus_all(KRef iterable, KRef tail) {
    KRef growing = kt_iterable_walked_into(iterable, kt_mutable_list_new());
    if (kt_raised()) {
        return NULL;
    }
    growing = kt_iterable_walked_into(tail, growing);
    if (kt_raised()) {
        return NULL;
    }
    return kt_frozen(growing, 0);
}

/* `xs.sumOf { … }`. Kotlin declares one per width the selector may answer, and the answer's TYPE
   is the selector's — so which of these a call reaches is decided where the declaration is in
   sight, and each unboxes what `invoke` hands back at the width its own name says. Summing at one
   width and narrowing afterwards would not do: `Int` addition wraps and `Long` addition does not,
   and a program that sums to an overflow is entitled to Kotlin's answer. */
/* The selector's answer for the next element, or NULL when the walk or the selector raised --
   which `kt_raised` then tells the caller, before it unboxes anything. */
static KRef kt_select_next(KRef iterator, KRef selector) {
    KRef element = kt_iterator_next(iterator);
    if (kt_raised()) {
        return NULL;
    }
    return kt_invoke_one(selector, element);
}

kt_int kt_iterable_sum_of_int(KRef iterable, KRef selector) {
    KRef iterator = kt_iterable_iterator(iterable);
    kt_int total = 0;
    while (kt_more(iterator)) {
        KRef selected = kt_select_next(iterator, selector);
        if (kt_raised()) {
            return 0;
        }
        total = (kt_int)((uint32_t)total + (uint32_t)kt_unbox_int(selected));
    }
    return kt_raised() ? 0 : total;
}

kt_long kt_iterable_sum_of_long(KRef iterable, KRef selector) {
    KRef iterator = kt_iterable_iterator(iterable);
    kt_long total = 0;
    while (kt_more(iterator)) {
        KRef selected = kt_select_next(iterator, selector);
        if (kt_raised()) {
            return 0;
        }
        total = (kt_long)((uint64_t)total + (uint64_t)kt_unbox_long(selected));
    }
    return kt_raised() ? 0 : total;
}

kt_double kt_iterable_sum_of_double(KRef iterable, KRef selector) {
    KRef iterator = kt_iterable_iterator(iterable);
    kt_double total = 0.0;
    while (kt_more(iterator)) {
        KRef selected = kt_select_next(iterator, selector);
        if (kt_raised()) {
            return 0.0;
        }
        total += kt_unbox_double(selected);
    }
    return kt_raised() ? 0 : total;
}

KRef kt_iterator_next(KRef iterator) {
    if (iterator == NULL) {
        kt_null_receiver();
        return NULL;
    }
    if (kt_is_list_iterator(iterator)) {
        return kt_list_iterator_next(iterator);
    }
    if (kt_walk_is(iterator)) {
        /* Asked before the read, because the array read does not check: an element past the end
           is whatever follows the storage. */
        if (kt_walk_exhausted(iterator)) {
            return NULL;
        }
        KWalk *walk = (KWalk *)iterator;
        if (iterator->header.type == &kt_type_chars_iterator) {
            return kt_box_char(kt_chars_next(walk));
        }
        kt_int at = walk->at;
        walk->at = at + 1;
        return kt_array_element(walk->over, at);
    }
    if (iterator->header.type == &kt_type_indexing_iterator) {
        KIndexingIterator *counting = (KIndexingIterator *)iterator;
        /* Kotlin's `IndexedValue(checkIndexOverflow(index++), iterator.next())`: the index is
           checked and bumped before the element is fetched. The element stays in a local across
           the allocation below so the collector sees it as a root. */
        uint32_t at = counting->at;
        if (kt_counter_overflowed(at, KT_COUNTS_INDICES)) {
            return NULL;
        }
        counting->at = at + 1;
        KRef element = kt_iterator_next(counting->source);
        if (kt_raised()) {
            return NULL;
        }
        return kt_indexed_value((kt_int)at, element);
    }
    if (iterator->header.type->walk_next != NULL) {
        return iterator->header.type->walk_next(iterator);
    }
    kt_long value = kt_range_iterator_next(iterator);
    /* An exhausted range raised, and its zero is no element to box. */
    if (kt_raised()) {
        return NULL;
    }
    if (iterator->header.type == &kt_type_long_progression_iterator) {
        return kt_box_long(value);
    }
    if (iterator->header.type == &kt_type_char_progression_iterator) {
        return kt_box_char((kt_char)value);
    }
    if (iterator->header.type == &kt_type_uint_progression_iterator) {
        return kt_box_uint((kt_int)value);
    }
    if (iterator->header.type == &kt_type_ulong_progression_iterator) {
        return kt_box_ulong(value);
    }
    return kt_box_int((kt_int)value);
}

/* Kotlin's `List.equals`: elementwise equal, in order, and only against another `List` — which is
   an `is` against the interface, so a list class of the program's own is compared too. A list
   never equals a set with the same members, and a `List` equals a `MutableList` holding the same
   things. Each element answers through its own `equals`, and one that throws ends the comparison
   whatever it returned.

   Two of the runtime's lists compare by index. A program's list is walked through its own
   iterator, the one member of it this side can reach, the way the JVM's `AbstractList.equals`
   walks the other list: `hasNext`, then `next`, then this element's `equals` with it, element by
   element, and at the end the other list must have nothing left. Every call into the program is
   followed by a look for the exception it may have thrown. */
static kt_boolean kt_list_equals(KRef self, KRef other) {
    if (self == other) {
        return true;
    }
    if (!kt_is_instance(other, &kt_type_list_interface)) {
        return false;
    }
    kt_int size = kt_list_size(self);
    KRef left = ((const KList *)self)->elements;
    if (other->header.type == &kt_type_list || kt_is_mutable_list(other)) {
        if (size != kt_list_size(other)) {
            return false;
        }
        KRef right = ((const KList *)other)->elements;
        for (kt_int i = 0; i < size; i++) {
            kt_boolean equal = kt_equals(kt_elements_of(left)[i], kt_elements_of(right)[i]);
            if (kt_raised() || !equal) {
                return false;
            }
        }
        return true;
    }
    KRef walk = kt_iterable_iterator(other);
    for (kt_int i = 0; i <= size; i++) {
        kt_boolean more = kt_more(walk);
        if (kt_raised()) {
            return false;
        }
        if (i == size || !more) {
            return i == size && !more;
        }
        KRef theirs = kt_iterator_next(walk);
        if (kt_raised()) {
            return false;
        }
        kt_boolean equal = kt_equals(kt_elements_of(left)[i], theirs);
        if (kt_raised() || !equal) {
            return false;
        }
    }
    return false;
}

/* Kotlin's own: 1 folded with `31 * h + e.hashCode()`, a null element contributing 0. */
static kt_int kt_list_hash_code(KRef self) {
    KRef elements = ((const KList *)self)->elements;
    kt_int length = kt_list_size(self);
    uint32_t hash = 1;
    for (kt_int i = 0; i < length; i++) {
        kt_int element_hash = kt_hash_code(kt_elements_of(elements)[i]);
        /* A `hashCode` that threw ends the fold before its answer is folded in; the zero is no
           hash, and the caller finds the exception pending before it reads one. */
        if (kt_raised()) {
            return 0;
        }
        hash = 31u * hash + (uint32_t)element_hash;
    }
    return (kt_int)hash;
}

/* `[a, b, c]`, each element through its own `toString` — which is what makes this a loop over
   `kt_string_plus` rather than a render into one buffer: an element's rendering may itself
   allocate, and the joined text has to stay reachable across that. A list that holds itself
   renders that element as `(this Collection)`, Kotlin's `AbstractCollection.toString`, rather
   than recursing into its own `toString`. */
static KRef kt_list_to_string(KRef self) {
    KRef elements = ((const KList *)self)->elements;
    kt_int length = kt_list_size(self);
    KRef text = kt_string_utf8("[", 1);
    for (kt_int i = 0; i < length; i++) {
        if (i > 0) {
            text = kt_string_plus(text, kt_string_utf8(", ", 2));
        }
        KRef element = kt_elements_of(elements)[i];
        KRef rendered = element == self ? kt_string_utf8("(this Collection)", 17)
                                        : kt_to_string(element);
        /* A `toString` that threw leaves the rendering there: its answer is no text, and the
           elements after it are not asked for theirs. */
        if (kt_raised()) {
            return NULL;
        }
        text = kt_string_plus(text, rendered);
    }
    return kt_string_plus(text, kt_string_utf8("]", 1));
}
