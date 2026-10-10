/* krusty native runtime: what its translation units share and generated code never sees.

   `krusty_rt.h` is the contract generated code is written against. This header is the runtime's
   own: the layout of a built-in value, and the handful of helpers that `krusty_rt.c` defines and
   `krusty_collections.c` calls, or the other way round. Nothing outside `src/native/runtime/`
   includes it. */
#ifndef KRUSTY_INTERNAL_H
#define KRUSTY_INTERNAL_H

#include "krusty_rt.h"
#include "krusty_sys.h"

#define KT_FAIL(literal) KT_SYS_FAIL(literal)

/* The names of a class of the runtime's own: `simple` in `package` (which ends in its dot), both
   published as a member class's (`KType.class_names`), and the rendered name their join. Written
   as two literals so the simple name is its own text rather than the tail of a split. */
#define KT_NAMED(package, simple)                                                                  \
    .name = package simple, .name_length = sizeof(package simple) - 1,                             \
    .qualified_name = package simple, .qualified_name_length = sizeof(package simple) - 1,         \
    .simple_name = simple, .simple_name_length = sizeof(simple) - 1,                               \
    .class_names = KT_CLASS_NAMES_MEMBER

/* A class Kotlin declares as an anonymous object, such as what `Array.asList` answers: `rendered`
   is what its `toString` and class literal print, and it publishes neither reflection name. */
#define KT_ANONYMOUS(rendered)                                                                     \
    .name = rendered, .name_length = sizeof(rendered) - 1, .class_names = KT_CLASS_NAMES_ANONYMOUS

/* Every built-in value is one of these; the header's type says which. */
struct KObject {
    KObjectHeader header;
    union {
        struct {
            /* The heap byte array holding the text, or NULL when `bytes` points into static
               storage (a literal). This is the string type's one reference field: it is what
               keeps the text alive exactly as long as the string. */
            KRef storage;
            const char *bytes;
            kt_int byte_length;
            /* The text's length in UTF-16 units plus one, once `kt_string_length` has counted it;
               zero until then. A string's text never changes, so it is normally counted at most
               once. INT32_MAX units cannot be encoded with this sentinel and remain uncached. A
               count equal to `byte_length` says every byte is ASCII: `s[i]` is then byte `i`. */
            kt_int units_plus_one;
        } string;
        kt_byte byte_value;
        kt_short short_value;
        kt_int int_value;
        kt_long long_value;
        kt_char char_value;
        kt_boolean boolean_value;
        kt_float float_value;
        kt_double double_value;
    } as;
};

/* A thread that runs Kotlin (`krusty_threads.c`). The first two fields are stored by assembly, at
   the offsets that file asserts. */
struct KThread {
    /* While released: the callee-saved registers as the code that released it left them. */
    uintptr_t registers[KT_SAVED_REGISTERS];
    /* While released: the stack pointer of that code. Its Kotlin frames lie between this and
       `stack_bottom`. */
    uintptr_t saved_sp;
    uintptr_t stack_bottom;
    /* While released: its exception in flight, which `kt_pending` holds while it runs. */
    KRef pending;
    long tid;
    bool released;
    KThread *next;
    /* A thread the runtime started, until it first runs: what it runs, and its argument, which is
       a root until the thread takes it. */
    void (*start_routine)(KRef);
    KRef start_argument;
    /* Its start order, which names it in an uncaught report. */
    uint32_t number;
};

/* The roots a collection finds outside its own stack, which `krusty_threads.c` knows and
   `krusty_gc.c` marks: the bottom of the holder's stack (0 before `kt_runtime_init`), and every
   released thread's recorded registers, exception and stack, handed to the two scanners below. */
/* `kt_check_uncaught` for a thread of another name: report what is in flight, if anything, as
   Kotlin reports an exception nothing caught on `thread`, and end the process with 134. */
void kt_report_uncaught(const char *thread, size_t thread_length);

uintptr_t kt_threads_running_bottom(void);
void kt_threads_scan_released(void);
void kt_gc_scan_word(uintptr_t word);
void kt_gc_scan_range(uintptr_t low, uintptr_t high);

/* A `ByteArray`, the storage of a string's text, and its body. */
typedef KArray KByteArray;

static inline char *kt_bytes_of(KByteArray *array) { return (char *)(array + 1); }

KByteArray *kt_bytes_new(kt_int length);

/* A string naming `byte_length` bytes at `bytes`, kept alive by `storage` (NULL for static text). */
KRef kt_string_of(KRef storage, const char *bytes, kt_int byte_length);

/* The read-only list's layout, which every list shape begins with, and a reference array's
   elements. */
typedef struct KList {
    KObjectHeader header;
    KRef elements;
} KList;

static inline KRef *kt_elements_of(KRef array) { return (KRef *)((KArray *)array + 1); }

/* An array's element count, from the header every array begins with. */
static inline kt_int kt_length_of(KRef array) { return ((const KArray *)array)->length; }

/* `kotlin.Any`'s three members, the table of every runtime type that overrides none of them. */
extern const kt_fn kt_any_vtable[3];

/* A UTF-16 walk over UTF-8 storage, which is what every question Kotlin asks about a string's
   CONTENT needs: the unit is the unit Kotlin counts, and a character above U+FFFF is two of them.
   `pending` holds the trailing surrogate of a pair whose leading half has already been handed out;
   zero is not a valid trailing surrogate, so it doubles as "none". */
typedef struct KUnits {
    const char *bytes;
    kt_int byte_length;
    kt_int at;
    uint32_t pending;
} KUnits;

KUnits kt_units_of(KRef self);
/* The next unit, or zero when the text is exhausted. */
kt_boolean kt_units_next(KUnits *units, kt_char *out);

/* Whether a value is one of the runtime's ranges or progressions, and whether one is empty in the
   direction it walks. Defined with the ranges in `krusty_rt.c`. */
kt_boolean kt_is_range(KRef value);
kt_boolean kt_range_empty(const KRange *range);

/* The iterator a range or progression hands out, one per element kind, which says how to box what
   it yields. */
extern const KType kt_type_int_progression_iterator;
extern const KType kt_type_long_progression_iterator;
extern const KType kt_type_char_progression_iterator;
extern const KType kt_type_uint_progression_iterator;
extern const KType kt_type_ulong_progression_iterator;
/* `kotlin.collections.CharIterator`, the abstract class a text's iterator subclasses, and its
   `Int` and `Long` twins, which an array's iterator subclasses too. */
extern const KType kt_type_char_iterator;
extern const KType kt_type_int_iterator;
extern const KType kt_type_long_iterator;

/* The walks over arrays and strings, which a range iterator's entry points also answer for:
   whether an iterator is one, and its `hasNext` and `next` as a 64-bit value. Defined in
   `krusty_collections.c`. */
kt_boolean kt_walk_is(KRef iterator);
kt_boolean kt_walk_has_next(KRef iterator);
kt_long kt_walk_next_long(KRef iterator);

/* `function(first, second, third)` through the slot every function value declares. Defined in
   `krusty_collections.c`. */
KRef kt_invoke_three(KRef function, KRef first, KRef second, KRef third);

/* The value facilities `krusty_classes.c` defines for the rest of the runtime: a built-in value's
   `equals` and `hashCode`, an object's own `toString` through its vtable, the bits `equals` and
   `hashCode` read from a floating-point value (every NaN collapsed to one), and the raise a
   `notNull` delegate read before it was written makes. */
kt_boolean kt_builtin_equals(KRef self, KRef other);
kt_int kt_builtin_hash_code(KRef self);
KRef kt_object_to_string(KRef value);
uint64_t kt_double_bits(kt_double value);
uint32_t kt_float_bits(kt_float value);
void kt_raise_uninitialized_property(KRef name);

/* The size of a map, a set or one of a map's views, or -1 for anything else: what a walk sizing
   its result asks. Defined in `krusty_maps.c`. */
kt_int kt_map_collection_size(KRef value);

/* A built-in value of `type`, fields zeroed: the one allocation every box makes. Defined in
   `krusty_rt.c`. */
KRef kt_new(const KType *type);

/* The text `value` renders as, for `print`, string templates and the uncaught report: its bytes,
   their count through `byte_length`, and through `storage` what keeps them alive. An object of the
   program renders through its own `toString`, which may raise; the caller asks the pending slot
   before it reads the answer. Defined in `krusty_rt.c`. */
const char *kt_render(KRef value, kt_int *byte_length, KRef *storage);

/* An unsigned 64-bit value in decimal, into `buffer` (at least 20 bytes); answers the length
   written. Defined in `krusty_lang.c`. */
kt_int kt_render_ulong(uint64_t value, char *buffer);

#endif /* KRUSTY_INTERNAL_H */
