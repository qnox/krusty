/* krusty native runtime — generated; do not edit. */
#ifndef KRUSTY_RT_H
#define KRUSTY_RT_H

/* Freestanding headers only: C11 guarantees these exist with no C library present, which is what
   lets one host compile for every target architecture without a sysroot. */
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

/* The runtime defines these itself (`krusty_rt.c`), because a compiler may synthesize calls to
   them from ordinary assignments and loops even under -ffreestanding. Declared here so the
   runtime's other translation units can call them without a C library's `<string.h>`. */
void *memcpy(void *destination, const void *source, size_t length);
void *memset(void *destination, int value, size_t length);

typedef int8_t   kt_byte;
typedef int16_t  kt_short;
typedef int32_t  kt_int;
typedef int64_t  kt_long;
typedef uint16_t kt_char;
typedef float    kt_float;
typedef double   kt_double;
typedef bool     kt_boolean;

/* ---- object model ---------------------------------------------------------------------------- */

/* Every heap object begins with a pointer to its type. The type is what makes the heap PRECISELY
   traceable: it names the byte offset of every reference-typed field, so the collector follows
   exactly those and nothing else. A field holding an integer that happens to look like an address
   is never mistaken for a reference. */
/* A method implementation as stored in a vtable. Every slot is a function pointer of some exact
   signature; a call site casts to the signature it knows. A function-pointer-to-function-pointer
   cast is defined C, where a cast through `void *` is not. */
typedef void (*kt_fn)(void);

/* Named here so the walking members below can be typed; the reference typedefs come further on. */
struct KObject;

typedef struct KType {
    const char *name;                  /* qualified Kotlin name, for toString */
    uint32_t name_length;
    uint32_t instance_size;            /* bytes including the header; the fixed part, for arrays */
    uint32_t reference_count;          /* how many reference-typed fields */
    /* An array's element stride, or 0 for everything else. Elements begin at `instance_size`, so
       one object shape serves every array: the fixed part says where they start and this says how
       far apart they are. */
    uint32_t element_size;
    const uint32_t *reference_offsets; /* byte offset of each reference field */
    /* The superclass, or NULL for kotlin.Any only. `is` walks this chain. */
    const struct KType *super;
    /* Virtual dispatch: a class's table is its superclass's table with overridden slots replaced
       and newly declared methods appended, so a slot number assigned at the declaring class is
       valid for every subclass. The first three slots are kotlin.Any's (see KT_SLOT_*). */
    const kt_fn *vtable;
    uint32_t vtable_length;
    /* Whether an array's elements are references the collector must trace. Separate from
       `element_size` because a `LongArray`'s elements are the same width and must NOT be traced. */
    uint32_t element_references;
    /* Every interface this type implements, TRANSITIVELY — an interface's own bases included, and
       those of every superclass. `is` checks this list at each step of the super chain, which is
       why the list is flattened: an interface is not on the single-inheritance chain, so there is
       no second chain to walk. An interface's own descriptor carries its bases here too. */
    const struct KType *const *interfaces;
    uint32_t interface_count;
    /* On a CALLABLE REFERENCE's descriptor: the byte offset of the field holding the BOUND
       RECEIVER, or 0 when the reference binds none. Kept apart from `reference_offsets` because
       those are every reference-typed field the collector must trace, and a reference to a local
       function carries its ordinary captures among them — so the first of them is not the
       receiver, and equality reading it there compared a capture instead. 0 is unambiguous: no
       field can sit at offset 0, which is the type header. */
    uint32_t reference_receiver_offset;
    /* Non-NULL only on a CALLABLE REFERENCE's descriptor, where it is the identity of the
       declaration referred to, together with whether a receiver is bound. Two `Foo::bar` written
       in two places are different objects with different descriptors, and Kotlin says they are
       EQUAL — so equality cannot be identity and cannot be the descriptor either. This is the
       thing they share. A bound `foo::bar` gets a different one from an unbound `Foo::bar`,
       because those must not be equal however much else they have in common. */
    const void *reference_target;
    /* On a class of the PROGRAM that implements `kotlin.collections.Iterable` or
       `kotlin.collections.Iterator`: how to ask an object of it for its iterator, or that iterator
       for its next element. NULL where the type answers for neither, which is every type but such
       a class.

       This is how the runtime walks an object it did not make. Every walking entry point below —
       `withIndex`, `contains`, `map`, `joinToString` — reaches its elements through
       `kt_iterable_iterator` and the two iterator members, and those know only the shapes this
       runtime builds.

       A POINTER rather than a vtable slot, because a slot alone would not say how to call what is
       in it: an emitted method has the signature its DECLARATION states, so `hasNext` answers an
       unboxed machine value and `next` answers whatever the element type is, which for an
       `Iterator<Int>` is not a reference at all. Each of these is a small emitted thunk with the
       fixed signature written here, which dispatches to the member VIRTUALLY — so a subclass
       overriding it is reached through the same thunk — and hands back what this side can read. */
    struct KObject *(*walk_iterator)(struct KObject *self);
    kt_boolean (*walk_has_next)(struct KObject *self);
    struct KObject *(*walk_next)(struct KObject *self);
    /* The same for a class of the program that implements `kotlin.CharSequence`, which is walked
       by its LENGTH and its indexed read rather than by an iterator — Kotlin's `CharSequence`
       declares no `iterator` at all. `kt_string_length` and `kt_string_get` reach these for an
       object that is neither a string nor a builder, which is what lets every question this
       runtime answers about TEXT be asked of text the program wrote. */
    kt_int (*walk_length)(struct KObject *self);
    kt_char (*walk_char_at)(struct KObject *self, kt_int index);
} KType;


typedef struct KObjectHeader {
    const KType *type;
} KObjectHeader;

/* kotlin.Any's three members, in the order every vtable begins with. */
#define KT_SLOT_EQUALS 0u    /* kt_boolean (*)(KRef self, KRef other) */
#define KT_SLOT_HASH_CODE 1u /* kt_int (*)(KRef self) */
#define KT_SLOT_TO_STRING 2u /* KRef (*)(KRef self) — a kotlin.String */
/* A FUNCTION VALUE's own member, in the one slot it declares beyond kotlin.Any's three. The
   generator puts a lambda's and a callable reference's `invoke` there; nothing else in this runtime
   calls through it, and nothing but a function value is ever handed to something that does. */
#define KT_SLOT_INVOKE 3u /* KRef (*)(KRef self, …) */

/* ---- memory ---------------------------------------------------------------------------------- */

/* Record where the program's stack begins. Roots are found by scanning the stack from the
   collector's own frame up to this address, so it must be called from the outermost frame BEFORE
   anything allocates; the generated entry point does so with the address of a local. */
void kt_runtime_init(void *stack_bottom);

/* Allocate `size` zeroed bytes (at least the header) for an object of `type`, collecting first if
   enough has been allocated since the last collection. Never returns NULL: exhaustion exits. */
void *kt_gc_allocate(const KType *type, uint32_t size);

/* Run a collection now. Automatic collections happen inside kt_gc_allocate. */
void kt_gc_collect(void);

/* Register a global slot that may hold a reference, so it is treated as a root. Static storage
   is not scanned — a freestanding program has no portable way to find its own data section. */
void kt_gc_add_global_root(void **slot);

/* Introspection, for tests: allocated objects, bytes mapped for the heap, and bytes held by
   allocated objects. */
size_t kt_gc_live_objects(void);
size_t kt_gc_heap_bytes(void);
size_t kt_gc_live_bytes(void);

/* ---- values ---------------------------------------------------------------------------------- */

/* Every Kotlin reference is one of these. `NULL` is Kotlin's `null`. */
typedef struct KObject KObject;
typedef KObject *KRef;

/* ---- classes ----------------------------------------------------------------------------------- */

/* The root of every class, defined by the runtime. Its vtable holds the defaults: `equals` is
   reference identity, `hashCode` derives from the object's address (valid because the collector
   never moves an object), `toString` is `<qualified name>@<hex hashCode>`. */
extern const KType kt_type_any;

/* The runtime's built-in value types, so emitted code can test `is String` and `as Int?`. */
extern const KType kt_type_string;
extern const KType kt_type_byte;
extern const KType kt_type_short;
extern const KType kt_type_int;
extern const KType kt_type_long;
extern const KType kt_type_char;
extern const KType kt_type_boolean;
extern const KType kt_type_float;
extern const KType kt_type_double;
extern const KType kt_type_unit;

/* The companion object of a built-in type. Declared in no file krusty compiles and carrying no
   state — every member of one is a constant the frontend folds — so the only observable thing about
   it is its IDENTITY, which `o === Int.Companion` asks about. One static object per companion, with
   a descriptor of its own so that `Int.Companion === Long.Companion` is false. */
extern const KType kt_type_byte_companion;
extern const KType kt_type_short_companion;
extern const KType kt_type_int_companion;
extern const KType kt_type_long_companion;
extern const KType kt_type_char_companion;
extern const KType kt_type_boolean_companion;
extern const KType kt_type_float_companion;
extern const KType kt_type_double_companion;
extern const KType kt_type_string_companion;

KRef kt_byte_companion(void);
KRef kt_short_companion(void);
KRef kt_int_companion(void);
KRef kt_long_companion(void);
KRef kt_char_companion(void);
KRef kt_boolean_companion(void);
KRef kt_float_companion(void);
KRef kt_double_companion(void);
KRef kt_string_companion(void);

/* The four unsigned integers. Each is a value class over a signed primitive and is carried as the
   machine integer it wraps, so a descriptor of its own is the only thing keeping a BOXED one from
   being an `Int`: `1u as? Int` must fail, and printing one must not print the signed number sharing
   its bits. */
/* `kotlin.Number` and `kotlin.Comparable` exist only as descriptors to point at: no value has one
   as its own type, and an `is` against either is answered by the interface list of the box. */
extern const KType kt_type_number;
extern const KType kt_type_comparable;
/* `kotlin.CharSequence`, which a `String` and a `StringBuilder` both point at. */
extern const KType kt_type_char_sequence;
extern const KType kt_type_ubyte;
extern const KType kt_type_ushort;
extern const KType kt_type_uint;
extern const KType kt_type_ulong;

/* Every array: the header, the length, then `element_size` bytes per element beginning at the
   type's `instance_size`. One shape for `IntArray` and `Array<T>` alike — what differs is the
   stride and whether the collector looks inside. */
typedef struct KArray {
    KObjectHeader header;
    kt_int length;
} KArray;

/* Allocate a zeroed array of `length` elements. Zero is the right initial value for every element
   kind Kotlin has here: `0`, `false`, `\u0000`, `0.0` and `null` are all zero bits. */
KRef kt_array_new(const KType *type, kt_int length);

/* Copy `source`'s elements into `destination` at `at`, answering where the next element goes. What
   a spread needs: `f(a, *xs, b)` builds one array whose size only run time knows. */
kt_int kt_array_copy_into(KRef destination, kt_int at, KRef source);

/* An index outside `0 until size`. Kotlin throws IndexOutOfBoundsException; with no exception
   machinery yet the honest realization is a diagnosable exit. */
void kt_index_out_of_bounds(kt_int index, kt_int size);

/* `Enum.toString()`: the constant's name, read from the storage `kotlin.Enum` contributes. */
KRef kt_enum_to_string(KRef self);

/* The failure an exhaustive `when` makes when none of its branches matched after all. */
void kt_no_when_branch_matched(void);

/* The failure `Color.valueOf` makes when no constant has that name. */
void kt_no_such_enum_constant(KRef name);

/* The array types, all runtime-owned: an array's descriptor depends on its element WIDTH, not on
   the element type a program wrote, so `Array<String>` and `Array<Foo>` share one. */
extern const KType kt_type_array; /* Array<T>: references */
extern const KType kt_type_byte_array;
extern const KType kt_type_short_array;
extern const KType kt_type_int_array;
extern const KType kt_type_long_array;
extern const KType kt_type_char_array;
extern const KType kt_type_boolean_array;
extern const KType kt_type_float_array;
extern const KType kt_type_double_array;
extern const KType kt_type_ubyte_array;
extern const KType kt_type_ushort_array;
extern const KType kt_type_uint_array;
extern const KType kt_type_ulong_array;

/* ---- lists --------------------------------------------------------------------------------- */

/* `listOf(...)` as a value: an immutable list over the `Array<T>` a vararg call already built,
   which is what Kotlin's own `listOf(vararg)` wraps too. Being immutable is what makes sharing
   that array sound — nothing a program can write through reaches it. `MutableList` is a different
   type and is not one of these. */
/* `kotlin.Function` and its arities. A function value is an object of a type of its own — one per
   lambda or callable reference — so an `is` against a function type cannot ask about that type. It
   asks about these markers, which every function value's descriptor names.

   Arity is what separates them: `f is Function0<*>` must be false of a `Function1`. Both the arity
   and the bare `Function` are named on each descriptor, because `KType.interfaces` is flattened
   and an interface's own bases are not walked. */
extern const KType kt_type_function;
extern const KType kt_type_function0;
extern const KType kt_type_function1;
extern const KType kt_type_function2;
extern const KType kt_type_function3;
extern const KType kt_type_function4;
extern const KType kt_type_function5;
extern const KType kt_type_function6;
extern const KType kt_type_function7;
extern const KType kt_type_function8;
extern const KType kt_type_function9;
extern const KType kt_type_function10;
extern const KType kt_type_function11;
extern const KType kt_type_function12;
extern const KType kt_type_function13;
extern const KType kt_type_function14;
extern const KType kt_type_function15;
extern const KType kt_type_function16;
extern const KType kt_type_function17;
extern const KType kt_type_function18;
extern const KType kt_type_function19;
extern const KType kt_type_function20;
extern const KType kt_type_function21;
extern const KType kt_type_function22;

/* Kotlin's reflection hierarchy, as far as a PROPERTY REFERENCE wears it. Each reference object
   has a type of its own — one per property — so an `is` against `KProperty0` cannot ask about that
   type and asks about these instead. The whole chain is named on each descriptor, because
   `KType.interfaces` is flattened and an interface's own bases are not walked. */
extern const KType kt_type_kcallable;
extern const KType kt_type_kproperty;
extern const KType kt_type_kproperty0;
extern const KType kt_type_kproperty1;
extern const KType kt_type_kproperty2;
extern const KType kt_type_kmutable_property;
extern const KType kt_type_kmutable_property0;
extern const KType kt_type_kmutable_property1;
extern const KType kt_type_kmutable_property2;

/* `kotlin.collections.List` as an `is` asks about it. The runtime builds two kinds of list — the
   immutable one `listOf` answers and the growable one `ArrayList()` answers — and a check names
   neither of those types, so both name this marker instead. It has no instances of its own. */
extern const KType kt_type_list_interface;

extern const KType kt_type_list;
extern const KType kt_type_list_iterator;

KRef kt_list_of(KRef elements);
KRef kt_list_empty(void);
/* `xs.toList()` and `xs.reversed()` on an ARRAY: a snapshot of its elements as a list, each boxed
   on the way in. A snapshot rather than a view, which is Kotlin's own answer and is observable —
   writing through the array afterwards leaves the list as it was. */
KRef kt_array_to_list(KRef array);
KRef kt_array_reversed(KRef array);

/* `xs.reversedArray()`: a new ARRAY of the same element type, backwards, elements copied by the
   descriptor's stride so a primitive array stays primitive. */
KRef kt_array_reversed_array(KRef array);
/* An annotation member's array, by CONTENT — what Kotlin gives an annotation instance's `equals`,
   `hashCode` and `toString` for an array member, and what separates it from a data class's (that
   one compares arrays by identity). Each element is read through its box, so a `Float` element is
   compared and hashed on the total order. */
kt_boolean kt_array_content_equals(KRef left, KRef right);
kt_int kt_array_content_hash_code(KRef array);
KRef kt_array_content_to_string(KRef array);
KRef kt_list_single(KRef value);
kt_int kt_list_size(KRef list);
kt_boolean kt_list_is_empty(KRef list);
KRef kt_list_get(KRef list, kt_int index);
/* `first()` / `last()`; both raise `NoSuchElementException` on an empty list, as Kotlin does. */
KRef kt_list_first(KRef list);
KRef kt_list_last(KRef list);
kt_int kt_list_index_of(KRef list, KRef value);
kt_int kt_list_last_index_of(KRef list, KRef value);
kt_boolean kt_list_contains(KRef list, KRef value);
KRef kt_list_iterator(KRef list);

/* ---- a growable list -------------------------------------------------------------------------

   `ArrayList`/`MutableList`: the same shape as an immutable list with a SIZE beside the storage,
   so every read above serves both. See the note on the definition. */
extern const KType kt_type_mutable_list;

kt_boolean kt_is_mutable_list(KRef value);
KRef kt_mutable_list_new(void);
KRef kt_mutable_list_with_capacity(kt_int capacity);
/* A growable list holding a COPY of an array's elements. */
KRef kt_mutable_list_of(KRef elements);
kt_boolean kt_mutable_list_add(KRef self, KRef value);
void kt_mutable_list_add_at(KRef self, kt_int index, KRef value);
/* Answers the element that was there, as Kotlin's `set` does. */
KRef kt_mutable_list_set(KRef self, kt_int index, KRef value);
KRef kt_mutable_list_remove_at(KRef self, kt_int index);
kt_boolean kt_mutable_list_remove(KRef self, KRef value);
void kt_mutable_list_clear(KRef self);
/* `list += element` and `list += elements`: Kotlin's `plusAssign`, which is `add`/`addAll` and
   answers `Unit`. */
void kt_mutable_list_plus_assign(KRef self, KRef value);
void kt_mutable_list_add_all(KRef self, KRef elements);
kt_boolean kt_list_iterator_has_next(KRef iterator);
KRef kt_list_iterator_next(KRef iterator);

/* Iteration through a receiver the generator could only type by the INTERFACE, where either of the
   two iterable things this runtime has may turn up. The descriptor decides which, and `next`
   answers a reference because an interface-typed receiver has its element type erased. */
KRef kt_iterable_iterator(KRef iterable);
kt_boolean kt_iterator_has_next(KRef iterator);
KRef kt_iterator_next(KRef iterator);

/* The two walks a program hands a function value. Declared on `Iterable`, so they are answered for
   whichever iterable the receiver holds, by the same dispatch iteration itself goes through. */
KRef kt_iterable_map(KRef iterable, KRef transform);

/* `xs.joinToString()` with every parameter left at its default: `", "` between the elements,
   nothing around them, no limit, and each element rendered by its own `toString`. Only that form
   is realized; a call that passes anything declines at the call site, where the argument it passed
   is still visible. */
KRef kt_iterable_join_to_string(KRef iterable);

/* `xs.withIndex()` and the pair it yields.
   The result is an ITERABLE, not a list: `withIndex` is lazy in Kotlin, and the walk it wraps may
   be one a program stops early. Iterating it counts as it goes and yields one `IndexedValue` per
   element — a Kotlin data class, so its `equals`, `hashCode` and `toString` are the data class's
   and not identity's. */
KRef kt_iterable_with_index(KRef iterable);

/* `xs.asSequence()`: the source, kept until somebody asks for an iterator. Lazy like `withIndex`,
   and its own type — what separates a `Sequence` from an `Iterable` here is which members may be
   asked of it. Every walk this runtime has is EAGER, and an eager `map` on a sequence is not
   Kotlin's, so a sequence is offered only its iterator and the lazy `withIndex`; the rest decline
   at the call site. `equals`/`hashCode` are identity, which is what `Sequence` answers. */
extern const KType kt_type_sequence;
KRef kt_sequence_of(KRef source);
kt_boolean kt_is_sequence(KRef value);
KRef kt_indexed_value(kt_int index, KRef value);
kt_int kt_indexed_value_index(KRef self);
KRef kt_indexed_value_value(KRef self);
void kt_iterable_for_each(KRef iterable, KRef action);

/* The other walks Kotlin declares over `Iterable`. Each takes the receiver and, where it has one,
   the function value the program wrote — whose answer arrives boxed, because a function value's
   `invoke` hands back a reference whatever its declared return type is.

   `any`, `all` and `none` stop at the first element that settles the question, which Kotlin
   promises and a predicate with a side effect can observe. `filter` asks its predicate once per
   element for the same reason. `first`/`last` with a predicate raise `NoSuchElementException` when
   nothing matches; the `OrNull` form answers NULL. */
kt_boolean kt_iterable_any(KRef iterable, KRef predicate);
kt_boolean kt_iterable_all(KRef iterable, KRef predicate);
kt_boolean kt_iterable_none(KRef iterable, KRef predicate);
kt_boolean kt_iterable_is_not_empty(KRef iterable);
kt_boolean kt_iterable_is_empty(KRef iterable);
kt_int kt_iterable_count(KRef iterable);
kt_int kt_iterable_count_matching(KRef iterable, KRef predicate);
KRef kt_iterable_filter(KRef iterable, KRef predicate);
KRef kt_iterable_filter_not(KRef iterable, KRef predicate);
KRef kt_iterable_first_matching(KRef iterable, KRef predicate);
KRef kt_iterable_first_or_null(KRef iterable, KRef predicate);
KRef kt_iterable_last_matching(KRef iterable, KRef predicate);
KRef kt_iterable_fold(KRef iterable, KRef initial, KRef operation);
void kt_iterable_for_each_indexed(KRef iterable, KRef action);
KRef kt_iterable_to_list(KRef iterable);
/* `list.sortWith(comparator)` in place, and `xs.sortedWith(comparator)` answering a new list. The
   comparator is an ordinary function value of two arguments — a `Comparator` this runtime makes is
   a `Function2`, nothing but its one member ever being asked of it — so the comparison goes through
   the same invoke slot every function value declares. STABLE, as Kotlin's are. */
void kt_list_sort_with(KRef list, KRef comparator);
/* `comparator.compare(a, b)` written out by a program: the same invoke the sort makes, with the
   boxed answer unwrapped to the `Int` the call site asked for. */
kt_int kt_comparator_compare(KRef comparator, KRef left, KRef right);
KRef kt_iterable_sorted_with(KRef iterable, KRef comparator);
KRef kt_iterable_reversed(KRef iterable);
kt_int kt_iterable_index_of(KRef iterable, KRef value);
kt_boolean kt_iterable_contains(KRef iterable, KRef value);

/* `xs + x` and `xs + ys`: a NEW read-only list. Which one a call means is decided by the CALLER
   from the declaration's physical parameter, as it is for `plusAssign`. */
/* `xs.sumOf { … }`, one per width the selector may answer: the answer's type is the selector's,
   and summing at one width and narrowing afterwards would give a different answer on overflow. */
kt_int kt_iterable_sum_of_int(KRef iterable, KRef selector);
kt_long kt_iterable_sum_of_long(KRef iterable, KRef selector);
kt_double kt_iterable_sum_of_double(KRef iterable, KRef selector);

KRef kt_iterable_plus_element(KRef iterable, KRef element);
KRef kt_iterable_plus_all(KRef iterable, KRef tail);

/* ---- lazy ---------------------------------------------------------------------------------- */

/* `by lazy { … }`: the initializer until it has run, the value afterwards, and the one bit that
   says which. The initializer is dropped once it has run, as Kotlin's own `SynchronizedLazyImpl`
   does — it is no longer reachable from the program, so keeping it would keep its captures alive
   for nothing.

   Single-threaded for now: this target has no threads, and Kotlin's default mode synchronizes. When
   threads arrive, this is where that lock goes. */
extern const KType kt_type_lazy;

KRef kt_lazy_of(KRef initializer);
KRef kt_lazy_value(KRef lazy);
kt_boolean kt_lazy_is_initialized(KRef lazy);

/* ---- pairs --------------------------------------------------------------------------------- */

/* `a to b`: two references, with the three `kotlin.Any` members answering componentwise the way
   Kotlin's data class does. `Triple` is not one of these and is not realized. */
extern const KType kt_type_pair;

KRef kt_pair_of(KRef first, KRef second);
KRef kt_pair_first(KRef pair);
KRef kt_pair_second(KRef pair);

/* ---- ranges ---------------------------------------------------------------------------------- */

/* `1..3`, `a..<b`, `'a'..'z'` as a VALUE. The three closed integral ranges share one shape — two
   bounds — and differ only by descriptor, which is what `equals`, `hashCode` and `toString` read to
   answer the way the Kotlin declaration each stands for does. The bounds are kept at 64 bits for
   all three so one struct serves them; the narrower two are stored sign- or zero-extended exactly
   as their Kotlin type reads them, so a comparison at this width answers what one at their own
   width would. */
typedef struct KRange {
    KObjectHeader header;
    kt_long first;
    /* The last ELEMENT, already brought onto the step: `1..10 step 3` ends at 10 and `1..9 step 3`
       ends at 7, so iterating is "walk from `first` by `step` until past `last`" with no remainder
       to think about at each turn. Kotlin normalizes the same way, and it is observable —
       `(1..9 step 3).last` is 7. */
    kt_long last;
    /* Never zero. `1` for a plain range, so one struct serves a range and a progression and every
       reader below serves both; `a downTo b` and `reversed()` make it negative. */
    kt_long step;
} KRange;

extern const KType kt_type_int_range;
extern const KType kt_type_long_range;
extern const KType kt_type_char_range;
/* Kotlin declares only these two unsigned ranges: `UByte.rangeTo` and `UShort.rangeTo` both answer
   a `UIntRange`, so the four unsigned scalars need no more than the pair. Their bounds are read
   UNSIGNED, which for `ULongRange` is the difference between `18446744073709551615uL` and `-1`. */
extern const KType kt_type_uint_range;
extern const KType kt_type_ulong_range;

/* `first..last`. An empty range is one whose `first` exceeds its `last`, which is a value, not an
   error: `3..1` is the empty range and Kotlin says so. */
KRef kt_int_range(kt_int first, kt_int last);
KRef kt_long_range(kt_long first, kt_long last);
KRef kt_char_range(kt_char first, kt_char last);

/* `first..<last` / `first until last`. Kotlin answers the EMPTY range rather than wrapping when
   `last` is the element type's minimum, so the half-open form is a function and not `last - 1`. */
KRef kt_int_range_until(kt_int first, kt_int last);
KRef kt_long_range_until(kt_long first, kt_long last);
KRef kt_char_range_until(kt_char first, kt_char last);

/* The unsigned pair, bounds already zero-extended by the caller. `until` answers the empty range
   at a last bound of ZERO — an unsigned type's minimum — rather than at the signed minimum. */
KRef kt_uint_range(kt_int first, kt_int last);
KRef kt_ulong_range(kt_long first, kt_long last);
KRef kt_uint_range_until(kt_int first, kt_int last);
KRef kt_ulong_range_until(kt_long first, kt_long last);

/* `value in range`. The caller widens its own element to 64 bits — signed for `Int` and `Long`,
   unsigned for `Char` — which is the same widening the bounds were stored with. */
kt_boolean kt_range_contains(KRef range, kt_long value);
/* `range.first` / `range.last` / `range.start` / `range.endInclusive`, at the range's own width. */
kt_long kt_range_first(KRef range);
kt_long kt_range_last(KRef range);
kt_boolean kt_range_is_empty(KRef range);

/* ---- floating-point ranges -------------------------------------------------------------------

   `0.0..2.0`. Not a progression: it has no step and no walk, because there is no next
   floating-point number for Kotlin to name — it is a pair of bounds and the question `value in it`.
   A `Float` range is stored at `double`, which is exact and order-preserving, so both widths share
   one shape and the descriptor remembers which. NaN needs no case of its own: `contains` is
   `value >= start && value <= end` and `isEmpty` is `!(start <= end)`, which is Kotlin's own
   `lessThanOrEquals` on these types. */
extern const KType kt_type_double_range;
extern const KType kt_type_float_range;

KRef kt_double_range(kt_double start, kt_double end);
KRef kt_float_range(kt_float start, kt_float end);
kt_boolean kt_floating_range_contains(KRef range, kt_double value);
kt_boolean kt_floating_range_is_empty(KRef range);
kt_double kt_floating_range_start(KRef range);
kt_double kt_floating_range_end(KRef range);

/* ---- Delegates.notNull -------------------------------------------------------------------------

   `var x: T by Delegates.notNull()`: one reference, and reading it before it is written is an
   error. The NAME the error reports is handed over by the caller, read from the `KProperty`
   operand the delegate convention passes. */
extern const KType kt_type_not_null_var;

KRef kt_not_null_var(void);
/* `Delegates.observable(initial) { property, old, new -> … }`: the value and a callback run after
   each write. The `KProperty` is passed along, never read, which is what lets this carry it without
   any reflection. */
extern const KType kt_type_observable;
KRef kt_observable(KRef initial, KRef on_change);
/* The two entry points a `ReadWriteProperty` receiver reaches; the DESCRIPTOR says which delegate
   it is, no static type separating them. `get` takes the property's NAME, the only thing it can
   need (the text of a `notNull` read-before-write error); `set` takes the PROPERTY, which an
   observable passes to its callback. */
KRef kt_rw_property_get(KRef self, KRef name);
void kt_rw_property_set(KRef self, KRef property, KRef value);

/* ---- comparable ranges ------------------------------------------------------------------------

   `"a".."c"`, and every other `a..b` ordered by `Comparable` rather than by a machine comparison.
   The bounds are OBJECTS and each comparison is the value's own, through `kt_compare_any`. Not a
   progression either: `Comparable` names no successor, so there is no step and no walk. */
extern const KType kt_type_comparable_range;

KRef kt_comparable_range(KRef start, KRef end);
kt_boolean kt_comparable_range_contains(KRef range, KRef value);
kt_boolean kt_comparable_range_is_empty(KRef range);
KRef kt_comparable_range_start(KRef range);
KRef kt_comparable_range_end(KRef range);

/* `for (x in range)` over a range the program materialized. The iterator is its own object because
   the loop reads it twice per step; a range iterated straight from a literal never becomes one,
   because common lowering turns that into a counted loop before this backend sees it. */
KRef kt_range_iterator(KRef range);

/* ---- progressions ---------------------------------------------------------------------------

   `step`, `downTo` and `reversed` answer a PROGRESSION, which here is a range with a step: the
   same object, so `first`, `last`, `isEmpty` and iteration are the ones above. A step of zero is
   Kotlin's `IllegalArgumentException`, and the step given to `step` is its magnitude — `a downTo b
   step 2` descends by two, because the receiver's direction is what decides. */
KRef kt_range_step(KRef range, kt_long step);
KRef kt_range_reversed(KRef range);
KRef kt_int_range_down_to(kt_int first, kt_int last);
KRef kt_long_range_down_to(kt_long first, kt_long last);
KRef kt_char_range_down_to(kt_char first, kt_char last);
KRef kt_uint_range_down_to(kt_int first, kt_int last);
KRef kt_ulong_range_down_to(kt_long first, kt_long last);
kt_boolean kt_range_iterator_has_next(KRef iterator);
kt_long kt_range_iterator_next(KRef iterator);

/* Defaults, callable directly for `super.toString()` and friends. */
kt_boolean kt_any_equals(KRef self, KRef other);
kt_int kt_any_hash_code(KRef self);
KRef kt_any_to_string(KRef self);

/* `a.equals(b)` and `a.hashCode()` through the receiver's vtable; null-safe in Kotlin's sense
   (`null` equals only `null`, and hashes to 0). */
kt_boolean kt_equals(KRef a, KRef b);
kt_int kt_hash_code(KRef value);

/* `obj is type`: false for null, else true when `type` is on the object's superclass chain. */
kt_boolean kt_is_instance(KRef object, const KType *type);
/* `obj as type?` — null passes; `obj as type` — null fails; `obj as? type` — the object or null.
   A failed cast exits loudly, naming both types: the placeholder for ClassCastException until the
   runtime has exceptions. */
KRef kt_cast(KRef object, const KType *type);
KRef kt_cast_non_null(KRef object, const KType *type);
KRef kt_safe_cast(KRef object, const KType *type);

/* The vtable entry for an abstract method: never reached in a type-correct program, but a loud
   failure rather than a jump through NULL. */
/* `x!!` — yields `x`, or fails when it is null. Kotlin throws a NullPointerException here; with no
   exception machinery yet the honest realization is a diagnosable exit. */
KRef kt_not_null(KRef value);

void kt_abstract_method_called(void);

/* The standard-library throws a program writes on purpose: `TODO()`, `error(message)`, and a
   failed `require`/`check`. There are no exceptions on this target yet, so each is the same
   diagnosable exit `!!` on null and a failed cast already give — and no program that could CATCH
   one of these compiles here, so nothing observable is lost by not raising it. The message is the
   program's own, rendered the way `"$message"` renders it. */
void kt_not_implemented(void);
void kt_not_implemented_reason(KRef reason);
void kt_illegal_state(KRef message);
void kt_require(kt_boolean value);
void kt_check(kt_boolean value);

/* `kotlin.collections.throwIndexOverflow()`, which an inline stdlib body splices into its caller:
   `throw ArithmeticException("Index overflow has happened.")`. */
void kt_throw_index_overflow(void);
/* Kotlin's `assert`, on the FAILING side only: the caller branches on the condition, which is what
   keeps `lazyMessage` from being computed on the passing path. The message arrives as the FUNCTION
   and is invoked here; NULL is the form that wrote none, whose text Kotlin fixes. */
void kt_assertion_failed(KRef lazy_message);
void kt_nothing_value_returned(void);
/* A member access on `null`: the placeholder for NullPointerException. */
void kt_null_receiver(void);

static inline const KType *kt_type_of(KRef object) {
    return ((const KObjectHeader *)object)->type;
}

/* The implementation of `slot` for `receiver`'s dynamic type. The call site casts the result to
   the slot's signature and passes `receiver` as the first argument. */
static inline kt_fn kt_dispatch(KRef receiver, uint32_t slot) {
    if (receiver == NULL) {
        kt_null_receiver();
    }
    return kt_type_of(receiver)->vtable[slot];
}

/* Construct a `String` over a UTF-8 literal. The bytes are borrowed, not copied: emitted code only
   ever passes string literals with static storage duration, and the string records that it owns
   no heap text. */
KRef kt_string_utf8(const char *bytes, kt_int byte_length);

/* `a + b` on strings, after both operands have been rendered. */
KRef kt_string_plus(KRef a, KRef b);

/* `String.length` — a count of UTF-16 CODE UNITS, which a UTF-8 string does not store. */
kt_int kt_string_length(KRef self);

/* `s[index]` — the UTF-16 code unit at `index`, walking the UTF-8 bytes to find it. */
kt_char kt_string_get(KRef self, kt_int index);

/* `s.substring(start, end)` and `s.subSequence(start, end)`, both by UTF-16 unit as Kotlin indexes.
   The result SHARES the receiver's storage — a substring is a view, and the text it names is
   already there. Slicing BETWEEN the halves of a surrogate pair is a loud failure: UTF-8 has no
   encoding for half a character, so there is no string to hand back. */
KRef kt_string_substring(KRef self, kt_int start, kt_int end);

/* `s.substring(start)`: from there to the end. */
KRef kt_string_substring_from(KRef self, kt_int start);

/* `a.compareTo(b)`, by UTF-16 code unit and with Java's magnitude: the difference of the first
   units that differ, or of the lengths when one string is a prefix of the other. */
kt_int kt_string_compare_to(KRef a, KRef b);

/* `s.removeSuffix(suffix)`: the receiver without it, or the receiver itself when it does not end
   there. Bytes settle it — UTF-8 is a prefix code, so two texts end the same way exactly when
   their trailing bytes do. */
KRef kt_string_remove_suffix(KRef self, KRef suffix);

/* `s.isEmpty()` / `s.isNotEmpty()`: a text has zero UTF-16 units exactly when it has zero bytes. */
kt_boolean kt_string_is_empty(KRef self);
kt_boolean kt_string_is_not_empty(KRef self);

/* `s.isBlank()` / `s.isNotBlank()`: empty, or whitespace all the way through. Whitespace is
   Kotlin's `Char.isWhitespace()`, which is the UNION of Java's `isWhitespace` and `isSpaceChar`. */
kt_boolean kt_string_is_blank(KRef self);
kt_boolean kt_string_is_not_blank(KRef self);

/* `s.trim()`, `s.trimStart()`, `s.trimEnd()`: the receiver without whitespace at the named end.
   Like `substring`, the result shares a STRING receiver's storage; a builder's text is copied,
   because a later `append` may replace the array it lives in. */
KRef kt_string_trim(KRef self);
KRef kt_string_trim_start(KRef self);
KRef kt_string_trim_end(KRef self);

/* `s.startsWith(prefix)`, `s.endsWith(suffix)`, `s.contains(other)`. Bytes settle all three, the
   way they settle `removeSuffix`. Only the case-SENSITIVE forms arrive: the generator declines
   `ignoreCase = true`, which asks about Unicode case folding rather than about text. */
kt_boolean kt_string_starts_with(KRef self, KRef prefix);
kt_boolean kt_string_ends_with(KRef self, KRef suffix);
kt_boolean kt_string_contains(KRef self, KRef other);

/* `s.repeat(n)`; a negative count is Kotlin's `IllegalArgumentException`. */
KRef kt_string_repeat(KRef self, kt_int count);

/* `s.reversed()`, by CHARACTER — a surrogate pair stays together, as Kotlin's own answer does. */
KRef kt_string_reversed(KRef self);

/* `s.first()` / `s.last()`; both raise `NoSuchElementException` on empty text, as Kotlin does. */
kt_char kt_string_first(KRef self);
kt_char kt_string_last(KRef self);

/* A string LITERAL, interned: equal literals are ONE object, as Kotlin promises. `slot` is a static
   per distinct text; see the definition for why the root is registered before the allocation. */
KRef kt_string_literal(const char *bytes, kt_int length, KRef *slot);

/* `Any?.toString()` — also what a string template calls on each interpolated value. */
KRef kt_to_string(KRef value);

/* ---- string builders ------------------------------------------------------------------------ */

/* `kotlin.text.StringBuilder`: a growable UTF-8 buffer. Its `equals` and `hashCode` are identity,
   which is what Kotlin answers -- only `toString` is its own. Every question about its CONTENT --
   `length`, `sb[i]`, iterating it, comparing it -- goes through the `String` entry points, which
   answer for either shape. */
extern const KType kt_type_string_builder;

/* `StringBuilder()` and `StringBuilder(capacity)`. The capacity is a hint. */
KRef kt_string_builder_new(void);
KRef kt_string_builder_with_capacity(kt_int capacity);

/* `StringBuilder(text)`: a builder holding a COPY of it. */
KRef kt_string_builder_with_text(KRef text);

/* `sb.append(value)`, answering the RECEIVER so a chain of them reads as one expression. The value
   is rendered the way `"$value"` would render it, through its own `toString`. */
KRef kt_string_builder_append(KRef self, KRef value);

/* `sb.appendLine(value)` and `sb.appendLine()`. The line separator is `\n` on every target, which
   is what Kotlin specifies rather than the platform's. */
KRef kt_string_builder_append_line(KRef self, KRef value);
KRef kt_string_builder_append_new_line(KRef self);

/* `sb.setLength(n)`, by UTF-16 unit as Kotlin counts: shorter truncates, longer pads with NUL. */
void kt_string_builder_set_length(KRef self, kt_int length);

/* Whether a value is one, for the entry points that serve both shapes. */
kt_boolean kt_is_string_builder(KRef value);

/* ---- maps and sets ---------------------------------------------------------------------------

   A map is two growable lists side by side: its keys in insertion order and the values beside them
   at the same positions. A SET is the same object with no values, which is what a `LinkedHashSet`
   is — a map whose values nothing reads.

   Lookup is LINEAR, by `equals`. Kotlin's is by hash, and the difference is speed and nothing
   else. What a hash map would not give is the ORDER, which is observable: `mapOf` answers a
   `LinkedHashMap`, whose iteration, `toString` and `keys` are in insertion order. The unordered
   spellings answer this object too, their order being unspecified.

   `keys`, `values` and `entries` are SNAPSHOTS where Kotlin's are views — the same trade
   `toList()` on an array makes, visible only to a program that keeps one across a write. */
extern const KType kt_type_map;
extern const KType kt_type_set;
extern const KType kt_type_map_entry;

kt_boolean kt_is_map(KRef value);
kt_boolean kt_is_set(KRef value);
/* The list of a map's keys, which for a set ARE its elements — so one walk serves both. */
KRef kt_map_keys_list(KRef self);

KRef kt_map_new(void);
KRef kt_set_new(void);
/* `mapOf(a to b, …)` / `setOf(a, …)`, from the array a vararg call already packed. The contents
   are copied in: the array belongs to the caller, and a map can be written through. */
KRef kt_map_of(KRef pairs);
KRef kt_set_of(KRef elements);
/* `mapOf(a to b)`: the one-pair form Kotlin declares beside the vararg one. */
KRef kt_map_of_pair(KRef pair);

kt_int kt_map_size(KRef self);
kt_boolean kt_map_is_empty(KRef self);
/* `m[k]`, NULL for an absent key — which is why Kotlin declares `Map.get` nullable. */
KRef kt_map_get(KRef self, KRef key);
KRef kt_map_get_or_default(KRef self, KRef key, KRef fallback);
/* `put` answers the value that was there; `set` is `m[k] = v` and answers `Unit`. An existing key
   keeps its POSITION, which is what a `LinkedHashMap` promises. */
KRef kt_map_put(KRef self, KRef key, KRef value);
void kt_map_set(KRef self, KRef key, KRef value);
KRef kt_map_remove(KRef self, KRef key);
void kt_map_clear(KRef self);
kt_boolean kt_map_contains_key(KRef self, KRef key);
kt_boolean kt_map_contains_value(KRef self, KRef value);
KRef kt_map_keys(KRef self);
KRef kt_map_values(KRef self);
KRef kt_map_entries(KRef self);

kt_boolean kt_set_contains(KRef self, KRef value);
kt_boolean kt_set_add(KRef self, KRef value);
kt_boolean kt_set_remove(KRef self, KRef value);

KRef kt_map_entry_key(KRef entry);
KRef kt_map_entry_value(KRef entry);

/* ---- exceptions ---------------------------------------------------------------------------- */

/* The `Throwable` hierarchy a `catch` clause names. Each link is Kotlin's own, so matching a clause
   is `kt_is_instance` against the clause's type and nothing more. */
extern const KType kt_type_throwable;
extern const KType kt_type_error;
extern const KType kt_type_not_implemented_error;
extern const KType kt_type_exception;
extern const KType kt_type_runtime_exception;
extern const KType kt_type_illegal_state_exception;
extern const KType kt_type_illegal_argument_exception;
extern const KType kt_type_assertion_error;
extern const KType kt_type_null_pointer_exception;
extern const KType kt_type_class_cast_exception;
extern const KType kt_type_index_out_of_bounds_exception;
extern const KType kt_type_arithmetic_exception;
extern const KType kt_type_unsupported_operation_exception;
extern const KType kt_type_number_format_exception;
extern const KType kt_type_no_such_element_exception;
extern const KType kt_type_concurrent_modification_exception;
extern const KType kt_type_uninitialized_property_access_exception;

/* Allocate one. `message` may be NULL, which is Kotlin's `null` message. */
KRef kt_throwable_new(const KType *type, KRef message);

/* `Throwable(message, cause)` and `Throwable(cause)`. The second fills the message from the cause,
   as Kotlin does, so the two one-argument constructors are not one function with a flag. */
KRef kt_throwable_new_with_cause(const KType *type, KRef message, KRef cause);
KRef kt_throwable_new_from_cause(const KType *type, KRef cause);

/* `cause?.toString()`, for a subclass of `Throwable` the generator lays out itself: it writes the
   two fields in place and needs the same rendering `kt_throwable_new_from_cause` performs. */
KRef kt_throwable_message_of_cause(KRef cause);

/* Its `message`, or NULL. */
KRef kt_throwable_message(KRef self);

/* `Throwable.cause`, or NULL where the constructor was given none. */
KRef kt_throwable_cause(KRef self);

/* ---- kotlin.Result ------------------------------------------------------------------------- */

/* A value class over `Any?` whose representation is Kotlin's: a success IS the value, and a
   failure is a marker holding the exception. Every entry below takes that representation. */
KRef kt_result_success(KRef value);
KRef kt_result_failure(KRef exception);
kt_boolean kt_result_is_failure(KRef value);
kt_boolean kt_result_is_success(KRef value);
KRef kt_result_get_or_null(KRef value);
KRef kt_result_exception_or_null(KRef value);
KRef kt_result_get_or_throw(KRef value);
KRef kt_result_to_string(KRef value);

/* `Throwable.toString()`: the qualified name, and `: message` after it when there is one. A class
   the SOURCE declares as a subclass of one of these inherits it through its own vtable, which is
   why it is published rather than private to this file. */
KRef kt_throwable_to_string(KRef self);

/* Reading a `lateinit` property before anything assigned it, named for the message Kotlin gives. */
void kt_uninitialized_property(KRef name);

/* `assertFailsWith<T> { … }` whose block threw the wrong thing, or nothing. `was` is what it threw,
   or NULL when it completed; `message` is the caller's prefix, or NULL. */
void kt_assert_failed_to_throw(KRef message, const KType *expected, KRef was);

/* `throw e`: record the exception in the one pending slot and RETURN. The caller's next act is
   `kt_pending_exception`, and that check is what turns the return into propagation. The slot is a
   GC root, because between the throw and the `catch` that names it the exception is reachable from
   no frame. See "How an exception propagates" in `docs/BUILD_AND_NATIVE_PLAN.md`. */
void kt_throw(KRef thrown);

/* The exception in flight, or NULL. Generated code LOADS this slot after every call rather than
   calling the accessor below: a call would clobber the caller-saved registers, and one after every
   call doubles what a frame keeps alive across a call boundary — which a deep enough recursion
   pays for in stack. */
extern KRef kt_pending;

/* The same slot through a call, for the runtime's own use. */
KRef kt_pending_exception(void);

/* A clause took it; nothing is in flight any more. */
void kt_clear_pending(void);

/* Nothing handled it: report it on stderr and end the program with 134, as Kotlin does. The
   generated entry calls this where it is about to treat `main`'s answer as an answer. */
void kt_check_uncaught(void);

/* ---- kotlin.test ------------------------------------------------------------------------------

   The assertions the box corpus checks itself with. Each raises the `AssertionError` Kotlin
   specifies, with Kotlin's wording, so a failing assertion reports what kotlinc would report.
   `message` may be NULL, which is the form without one. */
void kt_assert_equals(KRef expected, KRef actual, KRef message);
/* `assertSame`/`assertNotSame`: IDENTITY, which is what separates them from `assertEquals` — two
   strings with the same text are equal and are not the same object. */
void kt_assert_same(KRef expected, KRef actual, KRef message);
void kt_assert_not_same(KRef illegal, KRef actual, KRef message);
void kt_assert_true(kt_boolean actual, KRef message);
void kt_assert_false(kt_boolean actual, KRef message);

/* ---- callable references --------------------------------------------------------------------

   `kotlin.Any`'s identity equality is wrong for a callable reference: Kotlin promises that
   `Foo::bar == Foo::bar` and that `foo::bar == foo::bar`, while `foo::bar != bar::bar` and a bound
   reference never equals an unbound one. These answer all four from the descriptor's
   `reference_target` and the bound receiver, and they sit in the object's own vtable so a
   comparison through `Any` — which is how `x != y` on two `Any` parameters reaches here — gets the
   same answer as a comparison through the reference's own type. */
kt_boolean kt_reference_equals(KRef self, KRef other);
kt_int kt_reference_hash_code(KRef self);

/* `x::class` and `String::class`: a `kotlin.reflect.KClass` over one type descriptor.
   Two objects are EQUAL when they describe the same type, which is what Kotlin promises and what
   `x::class == String::class` asks; identity is not promised and is not relied on, so no table of
   canonical instances has to exist. The descriptor itself lives in static storage, so the object
   holds a pointer the collector neither traces nor needs to. */
extern const KType kt_type_kclass;
KRef kt_class_of(KRef value);
/* Named for the FORM rather than for what it takes: `kt_class_for` is the collector's own
   size-class helper, and a freestanding program links one namespace. */
KRef kt_class_literal(const KType *type);
/* `simpleName` and `qualifiedName`, read off the descriptor's own Kotlin name. */
KRef kt_class_simple_name(KRef self);
KRef kt_class_qualified_name(KRef self);

/* `Double.toString`/`Float.toString`: the shortest decimal that reads back as exactly this value,
   written into `out` (32 bytes is always enough). Returns the number of bytes written. Defined in
   `krusty_fp.c`, which is where the whole of that question lives. */
kt_int kt_render_double(kt_double value, char *out);
kt_int kt_render_float(kt_float value, char *out);

KRef kt_box_byte(kt_byte value);
KRef kt_box_short(kt_short value);
KRef kt_box_int(kt_int value);
KRef kt_box_long(kt_long value);
KRef kt_box_char(kt_char value);
KRef kt_box_boolean(kt_boolean value);
KRef kt_box_float(kt_float value);
KRef kt_box_double(kt_double value);
KRef kt_box_ubyte(kt_byte value);
KRef kt_box_ushort(kt_short value);
KRef kt_box_uint(kt_int value);
KRef kt_box_ulong(kt_long value);

kt_byte    kt_unbox_byte(KRef value);
kt_short   kt_unbox_short(KRef value);
kt_int     kt_unbox_int(KRef value);
kt_long    kt_unbox_long(KRef value);
kt_char    kt_unbox_char(KRef value);
kt_boolean kt_unbox_boolean(KRef value);
kt_float   kt_unbox_float(KRef value);
kt_double  kt_unbox_double(KRef value);
kt_byte    kt_unbox_ubyte(KRef value);
kt_short   kt_unbox_ushort(KRef value);
kt_int     kt_unbox_uint(KRef value);
kt_long    kt_unbox_ulong(KRef value);

/* `kotlin.Number`'s six conversions, on a value reached as an OBJECT.
   The site could type it only as a `Number`, so which primitive is in the box is the DESCRIPTOR's
   answer and not the call's — reading the bits as the wrong one is what these exist to prevent.
   Kotlin's own rules are kept: a floating-point source saturates and `NaN` answers zero, and a
   narrower integer target goes through `Int` first. Not a number at all is a loud failure; the
   frontend selected a member of `kotlin.Number`, so nothing else can be in the box. */
kt_byte   kt_number_to_byte(KRef value);
kt_short  kt_number_to_short(KRef value);
kt_int    kt_number_to_int(KRef value);
kt_long   kt_number_to_long(KRef value);
kt_float  kt_number_to_float(KRef value);
kt_double kt_number_to_double(KRef value);

/* `toString` on each of the four, reading the bits as the value they stand for. */
KRef kt_ubyte_to_string(kt_byte value);
KRef kt_ushort_to_string(kt_short value);
KRef kt_uint_to_string(kt_int value);
KRef kt_ulong_to_string(kt_long value);

/* Integer division, remainder and shifts. Kotlin defines all three; C leaves the interesting cases
   undefined. Division by zero throws in Kotlin and is undefined in C; `Int.MIN_VALUE / -1` overflows
   and is undefined in C but wraps in Kotlin; and a shift count outside 0..31 is undefined in C while
   Kotlin masks it to the low five (or six) bits. */
kt_int  kt_div_int(kt_int a, kt_int b);
kt_int  kt_rem_int(kt_int a, kt_int b);
kt_long kt_div_long(kt_long a, kt_long b);
kt_long kt_rem_long(kt_long a, kt_long b);
/* The unsigned pair. Only division by zero is undefined for unsigned operands — there is no
   `MIN_VALUE / -1` to wrap — so these are the signed helpers minus that case. */
kt_int  kt_div_uint(kt_int a, kt_int b);
kt_int  kt_rem_uint(kt_int a, kt_int b);
kt_long kt_div_ulong(kt_long a, kt_long b);
kt_long kt_rem_ulong(kt_long a, kt_long b);
/* `a.mod(b)` — the remainder carrying the DIVISOR's sign, where `%` carries the dividend's. Kotlin
   declares `mod` for every numeric pair; the narrow integers reach these two at `Int` width, which
   is exact because the answer's magnitude is below the divisor's. */
kt_int  kt_mod_int(kt_int a, kt_int b);
kt_long kt_mod_long(kt_long a, kt_long b);
kt_int  kt_shl_int(kt_int a, kt_int bits);
kt_int  kt_shr_int(kt_int a, kt_int bits);
kt_int  kt_ushr_int(kt_int a, kt_int bits);
kt_long kt_shl_long(kt_long a, kt_int bits);
kt_long kt_shr_long(kt_long a, kt_int bits);
kt_long kt_ushr_long(kt_long a, kt_int bits);

/* `compareTo` on scalars. Separate functions rather than an emitted `a < b ? -1 : ...` so neither
   operand is evaluated twice, and so the floating-point cases can implement Kotlin's TOTAL order
   (NaN above everything, -0.0 below 0.0) rather than C's comparison operators. */
/* `kotlin.math.abs`. The integral ones WRAP at the minimum, as Kotlin's do — there is no positive
   value to answer with. The floating ones clear the SIGN BIT rather than negating, so `abs(-0.0)` is
   `0.0`: `-0.0 < 0.0` is false, and a comparison-driven negation hands back what it was given. */
kt_int kt_abs_int(kt_int value);
kt_long kt_abs_long(kt_long value);
kt_float kt_abs_float(kt_float value);
kt_double kt_abs_double(kt_double value);

/* The bits of a floating-point value and back, a reinterpretation and nothing else. `toBits`
   differs from `toRawBits` in one respect: every NaN answers the canonical one, the same collapse
   `equals` and `hashCode` make. */
kt_int kt_float_to_raw_bits(kt_float value);
kt_long kt_double_to_raw_bits(kt_double value);
kt_int kt_float_to_bits(kt_float value);
kt_long kt_double_to_bits(kt_double value);
kt_float kt_float_from_bits(kt_int bits);
kt_double kt_double_from_bits(kt_long bits);

kt_int kt_compare_byte(kt_byte a, kt_byte b);
kt_int kt_compare_short(kt_short a, kt_short b);
kt_int kt_compare_int(kt_int a, kt_int b);
kt_int kt_compare_long(kt_long a, kt_long b);
kt_int kt_compare_char(kt_char a, kt_char b);
kt_int kt_compare_boolean(kt_boolean a, kt_boolean b);
/* `a % b` on floating point — IEEE's remainder truncated toward zero, which is what Kotlin's `%`
   means and what no instruction on some targets provides. Defined in `krusty_fp.c`. */
kt_float  kt_rem_float(kt_float a, kt_float b);
kt_double kt_rem_double(kt_double a, kt_double b);
/* `a.mod(b)` on floating point: the same remainder brought onto the DIVISOR's sign. Defined beside
   them in `krusty_fp.c`, because the sign it compares is Kotlin's `sign` — which answers NaN, and
   so is not the sign bit. */
kt_float  kt_mod_float(kt_float a, kt_float b);
kt_double kt_mod_double(kt_double a, kt_double b);

kt_int kt_compare_float(kt_float a, kt_float b);
kt_int kt_compare_double(kt_double a, kt_double b);

/* `a.compareTo(b)` where the static type says only `Comparable`. The DESCRIPTOR says what to
   compare, and only the orders the runtime defines are here: a boxed primitive at its own width
   (Kotlin's TOTAL order for the floating ones), a string by UTF-16 unit, the unsigned integers read
   unsigned. A program's own `Comparable` is not among them — a file that declares one declines at
   the call site. Two values of different types raise `ClassCastException`, as the JVM does. */
kt_int kt_compare_any(KRef a, KRef b);

/* The `kotlin.Unit` singleton. */
KRef kt_unit(void);

/* kotlin.io. The scalar overloads exist because Kotlin's do: `println(1)` selects `println(Int)`,
   and routing it through the `Any?` overload would box for no reason. */
void kt_print_any(KRef value);
void kt_print_byte(kt_byte value);
void kt_print_short(kt_short value);
void kt_print_int(kt_int value);
void kt_print_long(kt_long value);
void kt_print_char(kt_char value);
void kt_print_boolean(kt_boolean value);
void kt_print_float(kt_float value);
void kt_print_double(kt_double value);
void kt_print_ubyte(kt_byte value);
void kt_print_ushort(kt_short value);
void kt_print_uint(kt_int value);
void kt_print_ulong(kt_long value);

void kt_println_any(KRef value);
void kt_println_byte(kt_byte value);
void kt_println_short(kt_short value);
void kt_println_int(kt_int value);
void kt_println_long(kt_long value);
void kt_println_char(kt_char value);
void kt_println_boolean(kt_boolean value);
void kt_println_float(kt_float value);
void kt_println_double(kt_double value);
void kt_println_ubyte(kt_byte value);
void kt_println_ushort(kt_short value);
void kt_println_uint(kt_int value);
void kt_println_ulong(kt_long value);
void kt_println_unit(void);

/* The generated entry point calls this after running the program's `main`. */
void kt_exit(kt_int status);

#endif /* KRUSTY_RT_H */
