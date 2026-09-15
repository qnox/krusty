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

/* The four unsigned integers. Each is a value class over a signed primitive and is carried as the
   machine integer it wraps, so a descriptor of its own is the only thing keeping a BOXED one from
   being an `Int`: `1u as? Int` must fail, and printing one must not print the signed number sharing
   its bits. */
/* `kotlin.Number` and `kotlin.Comparable` exist only as descriptors to point at: no value has one
   as its own type, and an `is` against either is answered by the interface list of the box. */
extern const KType kt_type_number;
extern const KType kt_type_comparable;
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
extern const KType kt_type_list;
extern const KType kt_type_list_iterator;

KRef kt_list_of(KRef elements);
KRef kt_list_empty(void);
KRef kt_list_single(KRef value);
kt_int kt_list_size(KRef list);
kt_boolean kt_list_is_empty(KRef list);
KRef kt_list_get(KRef list, kt_int index);
kt_int kt_list_index_of(KRef list, KRef value);
kt_int kt_list_last_index_of(KRef list, KRef value);
kt_boolean kt_list_contains(KRef list, KRef value);
KRef kt_list_iterator(KRef list);
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
KRef kt_indexed_value(kt_int index, KRef value);
kt_int kt_indexed_value_index(KRef self);
KRef kt_indexed_value_value(KRef self);
void kt_iterable_for_each(KRef iterable, KRef action);

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
    kt_long last;
} KRange;

extern const KType kt_type_int_range;
extern const KType kt_type_long_range;
extern const KType kt_type_char_range;

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

/* `value in range`. The caller widens its own element to 64 bits — signed for `Int` and `Long`,
   unsigned for `Char` — which is the same widening the bounds were stored with. */
kt_boolean kt_range_contains(KRef range, kt_long value);
/* `range.first` / `range.last` / `range.start` / `range.endInclusive`, at the range's own width. */
kt_long kt_range_first(KRef range);
kt_long kt_range_last(KRef range);
kt_boolean kt_range_is_empty(KRef range);

/* `for (x in range)` over a range the program materialized. The iterator is its own object because
   the loop reads it twice per step; a range iterated straight from a literal never becomes one,
   because common lowering turns that into a counted loop before this backend sees it. */
KRef kt_range_iterator(KRef range);
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

/* `Any?.toString()` — also what a string template calls on each interpolated value. */
KRef kt_to_string(KRef value);

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
kt_int  kt_shl_int(kt_int a, kt_int bits);
kt_int  kt_shr_int(kt_int a, kt_int bits);
kt_int  kt_ushr_int(kt_int a, kt_int bits);
kt_long kt_shl_long(kt_long a, kt_int bits);
kt_long kt_shr_long(kt_long a, kt_int bits);
kt_long kt_ushr_long(kt_long a, kt_int bits);

/* `compareTo` on scalars. Separate functions rather than an emitted `a < b ? -1 : ...` so neither
   operand is evaluated twice, and so the floating-point cases can implement Kotlin's TOTAL order
   (NaN above everything, -0.0 below 0.0) rather than C's comparison operators. */
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

kt_int kt_compare_float(kt_float a, kt_float b);
kt_int kt_compare_double(kt_double a, kt_double b);

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
