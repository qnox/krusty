/* krusty native runtime — generated; do not edit. */
#ifndef KRUSTY_RT_H
#define KRUSTY_RT_H

/* Freestanding headers only: C11 guarantees these exist with no C library present, which is what
   lets one host compile for every target architecture without a sysroot. */
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

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
    const uint32_t *reference_offsets; /* byte offset of each reference field */
    /* The superclass, or NULL for kotlin.Any only. `is` walks this chain. */
    const struct KType *super;
    /* Virtual dispatch: a class's table is its superclass's table with overridden slots replaced
       and newly declared methods appended, so a slot number assigned at the declaring class is
       valid for every subclass. The first three slots are kotlin.Any's (see KT_SLOT_*). */
    const kt_fn *vtable;
    uint32_t vtable_length;
} KType;

typedef struct KObjectHeader {
    const KType *type;
} KObjectHeader;

/* kotlin.Any's three members, in the order every vtable begins with. */
#define KT_SLOT_EQUALS 0u    /* kt_boolean (*)(KRef self, KRef other) */
#define KT_SLOT_HASH_CODE 1u /* kt_int (*)(KRef self) */
#define KT_SLOT_TO_STRING 2u /* KRef (*)(KRef self) — a kotlin.String */

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
extern const KType kt_type_unit;

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

/* `Any?.toString()` — also what a string template calls on each interpolated value. */
KRef kt_to_string(KRef value);

/* No `kt_box_float`/`kt_box_double`: a floating-point value has no renderable form yet, so it must
   not be able to reach a position where something would try to render it. */
KRef kt_box_byte(kt_byte value);
KRef kt_box_short(kt_short value);
KRef kt_box_int(kt_int value);
KRef kt_box_long(kt_long value);
KRef kt_box_char(kt_char value);
KRef kt_box_boolean(kt_boolean value);

kt_byte    kt_unbox_byte(KRef value);
kt_short   kt_unbox_short(KRef value);
kt_int     kt_unbox_int(KRef value);
kt_long    kt_unbox_long(KRef value);
kt_char    kt_unbox_char(KRef value);
kt_boolean kt_unbox_boolean(KRef value);

/* Integer division, remainder and shifts. Kotlin defines all three; C leaves the interesting cases
   undefined. Division by zero throws in Kotlin and is undefined in C; `Int.MIN_VALUE / -1` overflows
   and is undefined in C but wraps in Kotlin; and a shift count outside 0..31 is undefined in C while
   Kotlin masks it to the low five (or six) bits. */
kt_int  kt_div_int(kt_int a, kt_int b);
kt_int  kt_rem_int(kt_int a, kt_int b);
kt_long kt_div_long(kt_long a, kt_long b);
kt_long kt_rem_long(kt_long a, kt_long b);
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

void kt_println_any(KRef value);
void kt_println_byte(kt_byte value);
void kt_println_short(kt_short value);
void kt_println_int(kt_int value);
void kt_println_long(kt_long value);
void kt_println_char(kt_char value);
void kt_println_boolean(kt_boolean value);
void kt_println_unit(void);

/* The generated entry point calls this after running the program's `main`. */
void kt_exit(kt_int status);

#endif /* KRUSTY_RT_H */
