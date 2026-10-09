/* krusty native runtime: `kotlin.*`'s arithmetic and unsigned integers, `Throwable` and the
   exception in flight, `kotlin.test`'s assertions, callable references, string literals and the
   console.

   A translation unit of its own, beside `krusty_rt.c`, so that no file of the runtime grows past
   what one file should hold. It shares the object layout and the rendering entry points through
   `krusty_internal.h`. */
#include "krusty_internal.h"

static kt_boolean kt_raised(void) { return kt_pending_exception() != NULL; }

/* ---- arithmetic ---------------------------------------------------------------------------- */

/* `a / 0`: Kotlin's `ArithmeticException`, with the JVM's wording, which a program may catch.
   Every caller RETURNS immediately after — the exception is recorded, not raised, so falling
   through would reach the machine divide this is here to avoid and take a SIGFPE. */
static void kt_divide_by_zero(void) {
    kt_throw(kt_throwable_new(&kt_type_arithmetic_exception, kt_string_utf8("/ by zero", 9)));
}

kt_int kt_div_int(kt_int a, kt_int b) {
    if (b == 0) {
        kt_divide_by_zero();
        return 0;
    }
    /* INT32_MIN / -1 overflows. Kotlin wraps to INT32_MIN; C leaves it undefined. */
    if (b == -1) {
        return (kt_int)(0u - (uint32_t)a);
    }
    return a / b;
}

kt_int kt_rem_int(kt_int a, kt_int b) {
    if (b == 0) {
        kt_divide_by_zero();
        return 0;
    }
    if (b == -1) {
        return 0;
    }
    return a % b;
}

kt_long kt_div_long(kt_long a, kt_long b) {
    if (b == 0) {
        kt_divide_by_zero();
        return 0;
    }
    if (b == -1) {
        return (kt_long)(0u - (uint64_t)a);
    }
    return a / b;
}

kt_long kt_rem_long(kt_long a, kt_long b) {
    if (b == 0) {
        kt_divide_by_zero();
        return 0;
    }
    if (b == -1) {
        return 0;
    }
    return a % b;
}

/* `a.mod(b)` — the remainder carrying the DIVISOR's sign, where `%` carries the dividend's. Kotlin
   defines it as `val r = a % b; if (r != 0 && r.sign != b.sign) r + b else r`, and the sign test is
   the exclusive-or's top bit. The addition is done on the unsigned ring: the sum fits the type by
   construction, and signed overflow would be undefined in C where Kotlin's wraps.

   Division by zero reaches `kt_rem_*` first, which records Kotlin's `ArithmeticException` and
   answers 0; the caller checks the pending slot before it reads anything. */
kt_int kt_mod_int(kt_int a, kt_int b) {
    kt_int r = kt_rem_int(a, b);
    if (r != 0 && (((uint32_t)r ^ (uint32_t)b) >> 31) != 0) {
        return (kt_int)((uint32_t)r + (uint32_t)b);
    }
    return r;
}

kt_long kt_mod_long(kt_long a, kt_long b) {
    kt_long r = kt_rem_long(a, b);
    if (r != 0 && (((uint64_t)r ^ (uint64_t)b) >> 63) != 0) {
        return (kt_long)((uint64_t)r + (uint64_t)b);
    }
    return r;
}

/* ---- unsigned integers ----------------------------------------------------------------------- */

/* The wrapped bits are stored in the signed field of the matching width; only the descriptor says
   how to read them. A small-value cache would be legitimate here too, but Kotlin promises nothing
   about the identity of a boxed unsigned value, so there is nothing to preserve by adding one. */
KRef kt_box_ubyte(kt_byte value) {
    KRef object = kt_new(&kt_type_ubyte);
    object->as.byte_value = value;
    return object;
}

KRef kt_box_ushort(kt_short value) {
    KRef object = kt_new(&kt_type_ushort);
    object->as.short_value = value;
    return object;
}

KRef kt_box_uint(kt_int value) {
    KRef object = kt_new(&kt_type_uint);
    object->as.int_value = value;
    return object;
}

KRef kt_box_ulong(kt_long value) {
    KRef object = kt_new(&kt_type_ulong);
    object->as.long_value = value;
    return object;
}

/* Unboxing a `null` is Kotlin's `NullPointerException`, the one `!!` raises. `kt_throw` records it
   and RETURNS, so each of these returns too — falling through would read the value out of the null
   just rejected. The zero is never read: the caller checks the pending slot first. */
kt_byte kt_unbox_ubyte(KRef value) {
    if (value == NULL) {
        kt_throw(kt_throwable_new(&kt_type_null_pointer_exception, NULL));
        return 0;
    }
    return value->as.byte_value;
}

kt_short kt_unbox_ushort(KRef value) {
    if (value == NULL) {
        kt_throw(kt_throwable_new(&kt_type_null_pointer_exception, NULL));
        return 0;
    }
    return value->as.short_value;
}

kt_int kt_unbox_uint(KRef value) {
    if (value == NULL) {
        kt_throw(kt_throwable_new(&kt_type_null_pointer_exception, NULL));
        return 0;
    }
    return value->as.int_value;
}

kt_long kt_unbox_ulong(KRef value) {
    if (value == NULL) {
        kt_throw(kt_throwable_new(&kt_type_null_pointer_exception, NULL));
        return 0;
    }
    return value->as.long_value;
}

/* Render an unsigned 64-bit value into `buffer` (at least 20 bytes); returns the length written.
   Separate from `kt_render_long` because the signed one negates into unsigned space to reach the
   digits, which is exactly the step that must not happen here. */
kt_int kt_render_ulong(uint64_t value, char *buffer) {
    char digits[20];
    kt_int count = 0;
    do {
        digits[count++] = (char)('0' + (value % 10u));
        value /= 10u;
    } while (value != 0);
    kt_int length = 0;
    while (count > 0) {
        buffer[length++] = digits[--count];
    }
    return length;
}

/* `toString` on each of the four. The narrow two are stored sign-extended in a `byte`/`short`, so
   the mask is what recovers the value from the bits. */
static KRef kt_unsigned_to_string(uint64_t value) {
    KByteArray *buffer = kt_bytes_new(24);
    kt_int length = kt_render_ulong(value, kt_bytes_of(buffer));
    return kt_string_of((KRef)buffer, kt_bytes_of(buffer), length);
}

KRef kt_ubyte_to_string(kt_byte value) { return kt_unsigned_to_string((uint8_t)value); }
KRef kt_ushort_to_string(kt_short value) { return kt_unsigned_to_string((uint16_t)value); }
KRef kt_uint_to_string(kt_int value) { return kt_unsigned_to_string((uint32_t)value); }
KRef kt_ulong_to_string(kt_long value) { return kt_unsigned_to_string((uint64_t)value); }

/* Division and remainder. Only division by zero is undefined for unsigned operands — there is no
   `MIN_VALUE / -1` to wrap — so this is the signed helpers minus that case. */
kt_int kt_div_uint(kt_int a, kt_int b) {
    if (b == 0) {
        kt_divide_by_zero();
        return 0;
    }
    return (kt_int)((uint32_t)a / (uint32_t)b);
}

kt_int kt_rem_uint(kt_int a, kt_int b) {
    if (b == 0) {
        kt_divide_by_zero();
        return 0;
    }
    return (kt_int)((uint32_t)a % (uint32_t)b);
}

kt_long kt_div_ulong(kt_long a, kt_long b) {
    if (b == 0) {
        kt_divide_by_zero();
        return 0;
    }
    return (kt_long)((uint64_t)a / (uint64_t)b);
}

kt_long kt_rem_ulong(kt_long a, kt_long b) {
    if (b == 0) {
        kt_divide_by_zero();
        return 0;
    }
    return (kt_long)((uint64_t)a % (uint64_t)b);
}

/* Kotlin masks the shift count, so `1 shl 32` is `1`, not undefined. A right shift of a negative
   value is implementation-defined in C, so the arithmetic shift is spelled out instead of assumed. */
kt_int kt_shl_int(kt_int a, kt_int bits) { return (kt_int)((uint32_t)a << (bits & 31)); }

kt_int kt_shr_int(kt_int a, kt_int bits) {
    uint32_t count = (uint32_t)(bits & 31);
    uint32_t shifted = (uint32_t)a >> count;
    if (a < 0 && count != 0) {
        shifted |= ~0u << (32 - count);
    }
    return (kt_int)shifted;
}

kt_int kt_ushr_int(kt_int a, kt_int bits) { return (kt_int)((uint32_t)a >> (bits & 31)); }

kt_long kt_shl_long(kt_long a, kt_int bits) { return (kt_long)((uint64_t)a << (bits & 63)); }

kt_long kt_shr_long(kt_long a, kt_int bits) {
    uint32_t count = (uint32_t)(bits & 63);
    uint64_t shifted = (uint64_t)a >> count;
    if (a < 0 && count != 0) {
        shifted |= ~(uint64_t)0 << (64 - count);
    }
    return (kt_long)shifted;
}

kt_long kt_ushr_long(kt_long a, kt_int bits) { return (kt_long)((uint64_t)a >> (bits & 63)); }

/* `kotlin.math.abs`. The integral ones WRAP at the minimum, as Kotlin's do: `abs(Int.MIN_VALUE)`
   is `Int.MIN_VALUE`, because there is no positive value to answer with. The unsigned arithmetic is
   what makes that defined rather than an overflow.

   The floating ones clear the SIGN BIT rather than negating: `abs(-0.0)` is `0.0` and `abs(NaN)` is
   a NaN, and a comparison-driven negation gets the first of those wrong — `-0.0 < 0.0` is false, so
   `x < 0 ? -x : x` hands back the negative zero it was given. */
kt_int kt_abs_int(kt_int value) {
    return value < 0 ? (kt_int)(0u - (uint32_t)value) : value;
}

kt_long kt_abs_long(kt_long value) {
    return value < 0 ? (kt_long)(0u - (uint64_t)value) : value;
}

kt_float kt_abs_float(kt_float value) {
    uint32_t bits;
    memcpy(&bits, &value, sizeof bits);
    bits &= 0x7FFFFFFFu;
    kt_float result;
    memcpy(&result, &bits, sizeof result);
    return result;
}

kt_double kt_abs_double(kt_double value) {
    uint64_t bits;
    memcpy(&bits, &value, sizeof bits);
    bits &= 0x7FFFFFFFFFFFFFFFULL;
    kt_double result;
    memcpy(&result, &bits, sizeof result);
    return result;
}

/* `x.toRawBits()` / `Float.fromBits(n)`: the bits as they are, in both directions. A
   reinterpretation and nothing else, which is why it is `memcpy` and not a cast — reading one type
   through a pointer to another is not defined C, and the copy compiles to no instruction.

   `toBits` differs from `toRawBits` in ONE respect: every NaN answers the canonical one, which is
   the same collapse `equals` and `hashCode` make and the reason `kt_double_bits` exists. */
kt_int kt_float_to_raw_bits(kt_float value) {
    uint32_t bits;
    memcpy(&bits, &value, sizeof bits);
    return (kt_int)bits;
}

kt_long kt_double_to_raw_bits(kt_double value) {
    uint64_t bits;
    memcpy(&bits, &value, sizeof bits);
    return (kt_long)bits;
}

kt_int kt_float_to_bits(kt_float value) { return (kt_int)kt_float_bits(value); }

kt_long kt_double_to_bits(kt_double value) { return (kt_long)kt_double_bits(value); }

kt_float kt_float_from_bits(kt_int bits) {
    uint32_t raw = (uint32_t)bits;
    kt_float value;
    memcpy(&value, &raw, sizeof value);
    return value;
}

kt_double kt_double_from_bits(kt_long bits) {
    uint64_t raw = (uint64_t)bits;
    kt_double value;
    memcpy(&value, &raw, sizeof value);
    return value;
}

#define KT_COMPARE(suffix, type)                                                                   \
    kt_int kt_compare_##suffix(type a, type b) { return a < b ? -1 : (a > b ? 1 : 0); }

KT_COMPARE(byte, kt_byte)
KT_COMPARE(short, kt_short)
KT_COMPARE(int, kt_int)
KT_COMPARE(long, kt_long)
KT_COMPARE(char, kt_char)
KT_COMPARE(boolean, kt_boolean)

#undef KT_COMPARE

/* Kotlin orders floating-point values totally, which `<` and `>` do not: every NaN compares
   greater than everything including itself, and -0.0 compares below 0.0. Falling back to the bit
   patterns after the ordinary comparisons reproduces that exactly, because IEEE-754 bits of
   like-signed values are monotonic. */
kt_int kt_compare_float(kt_float a, kt_float b) {
    if (a < b) {
        return -1;
    }
    if (a > b) {
        return 1;
    }
    int32_t left = 0;
    int32_t right = 0;
    memcpy(&left, &a, sizeof left);
    memcpy(&right, &b, sizeof right);
    if (a != a) {
        left = 0x7FC00000;
    }
    if (b != b) {
        right = 0x7FC00000;
    }
    return left == right ? 0 : (left < right ? -1 : 1);
}

kt_int kt_compare_double(kt_double a, kt_double b) {
    if (a < b) {
        return -1;
    }
    if (a > b) {
        return 1;
    }
    int64_t left = 0;
    int64_t right = 0;
    memcpy(&left, &a, sizeof left);
    memcpy(&right, &b, sizeof right);
    if (a != a) {
        left = 0x7FF8000000000000LL;
    }
    if (b != b) {
        right = 0x7FF8000000000000LL;
    }
    return left == right ? 0 : (left < right ? -1 : 1);
}

/* ---- exceptions ----------------------------------------------------------------------------

   A `Throwable` is an ordinary object with one reference field and a real `super` chain, which is
   the whole of what a `catch` needs: matching a clause is `kt_is_instance` against the clause's
   type, and that already walks this chain.

   `toString` is Kotlin's: the qualified name, and `: message` after it when there is one. A
   subclass declared in Kotlin source inherits this slot like any other. */

typedef struct KThrowable {
    KObjectHeader header;
    KRef message;
    /* The throwable this one was given as its cause, or NULL. Kotlin's `Throwable.cause` is a
       `val` set once by a constructor, so there is nothing to write after construction. */
    KRef cause;
} KThrowable;

static const uint32_t kt_throwable_offsets[] = {offsetof(KThrowable, message),
                                                offsetof(KThrowable, cause)};

KRef kt_throwable_to_string(KRef self) {
    const KType *type = self->header.type;
    KRef name = kt_string_utf8(type->name, (kt_int)type->name_length);
    KRef message = ((KThrowable *)self)->message;
    if (message == NULL) {
        return name;
    }
    return kt_string_plus(kt_string_plus(name, kt_string_utf8(": ", 2)), message);
}

static const kt_fn kt_throwable_vtable[] = {(kt_fn)kt_any_equals, (kt_fn)kt_any_hash_code,
                                            (kt_fn)kt_throwable_to_string};

/* The hierarchy a `catch` clause names. Each link is the one Kotlin declares, so
   `catch (e: Exception)` takes an `IllegalStateException` and does not take a bare `Throwable`. */
#define KT_THROWABLE_TYPE(identifier, package, simple, base)                                       \
    const KType identifier = {                                                                     \
        KT_NAMED(package, simple),                                                                 \
        .instance_size = sizeof(KThrowable),                                                       \
        /* `message` and `cause`, counted from the array rather than written out, so adding a      \
           field to `KThrowable` cannot leave one of them untraced. */                             \
        .reference_count =                                                                         \
            (uint32_t)(sizeof(kt_throwable_offsets) / sizeof(kt_throwable_offsets[0])),            \
        .reference_offsets = kt_throwable_offsets,                                                 \
        .super = base,                                                                             \
        .vtable = kt_throwable_vtable,                                                             \
        .vtable_length = 3};

KT_THROWABLE_TYPE(kt_type_throwable, "kotlin.", "Throwable", &kt_type_any)
KT_THROWABLE_TYPE(kt_type_error, "kotlin.", "Error", &kt_type_throwable)
KT_THROWABLE_TYPE(kt_type_not_implemented_error, "kotlin.", "NotImplementedError", &kt_type_error)
KT_THROWABLE_TYPE(kt_type_exception, "kotlin.", "Exception", &kt_type_throwable)
KT_THROWABLE_TYPE(kt_type_runtime_exception, "kotlin.", "RuntimeException", &kt_type_exception)
KT_THROWABLE_TYPE(kt_type_illegal_state_exception, "kotlin.", "IllegalStateException",
                  &kt_type_runtime_exception)
KT_THROWABLE_TYPE(kt_type_illegal_argument_exception, "kotlin.", "IllegalArgumentException",
                  &kt_type_runtime_exception)
KT_THROWABLE_TYPE(kt_type_assertion_error, "kotlin.", "AssertionError", &kt_type_error)
KT_THROWABLE_TYPE(kt_type_null_pointer_exception, "kotlin.", "NullPointerException",
                  &kt_type_runtime_exception)
KT_THROWABLE_TYPE(kt_type_class_cast_exception, "kotlin.", "ClassCastException",
                  &kt_type_runtime_exception)
KT_THROWABLE_TYPE(kt_type_index_out_of_bounds_exception, "kotlin.", "IndexOutOfBoundsException",
                  &kt_type_runtime_exception)
KT_THROWABLE_TYPE(kt_type_array_index_out_of_bounds_exception, "kotlin.",
                  "ArrayIndexOutOfBoundsException", &kt_type_index_out_of_bounds_exception)
KT_THROWABLE_TYPE(kt_type_arithmetic_exception, "kotlin.", "ArithmeticException",
                  &kt_type_runtime_exception)
KT_THROWABLE_TYPE(kt_type_unsupported_operation_exception, "kotlin.",
                  "UnsupportedOperationException", &kt_type_runtime_exception)
/* `NumberFormatException` is Kotlin's `IllegalArgumentException`, not a sibling of it. */
KT_THROWABLE_TYPE(kt_type_number_format_exception, "kotlin.", "NumberFormatException",
                  &kt_type_illegal_argument_exception)
KT_THROWABLE_TYPE(kt_type_no_such_element_exception, "kotlin.", "NoSuchElementException",
                  &kt_type_runtime_exception)
KT_THROWABLE_TYPE(kt_type_concurrent_modification_exception, "kotlin.",
                  "ConcurrentModificationException", &kt_type_runtime_exception)
KT_THROWABLE_TYPE(kt_type_uninitialized_property_access_exception, "kotlin.",
                  "UninitializedPropertyAccessException", &kt_type_runtime_exception)
KT_THROWABLE_TYPE(kt_type_no_when_branch_matched_exception, "kotlin.",
                  "NoWhenBranchMatchedException", &kt_type_runtime_exception)
KT_THROWABLE_TYPE(kt_type_read_after_eof_exception, "kotlin.io.", "ReadAfterEOFException",
                  &kt_type_runtime_exception)

KRef kt_throwable_new(const KType *type, KRef message) {
    /* `message` stays in this parameter across the allocation: it is its root.

       The size is the DESCRIPTOR's, not `KThrowable`'s: a subclass of `Throwable` lays its own
       fields out after these two, and an object allocated at the base size would have them overrun
       into whatever the heap put next. */
    KThrowable *thrown = (KThrowable *)kt_gc_allocate(type, type->instance_size);
    thrown->message = message;
    thrown->cause = NULL;
    return (KRef)thrown;
}

/* `Throwable(message, cause)`. Both operands stay in their parameters across the allocation, for
   the reason `message` alone does above: each is its own root, and the size is the descriptor's for
   the reason given there too. */
KRef kt_throwable_new_with_cause(const KType *type, KRef message, KRef cause) {
    KThrowable *thrown = (KThrowable *)kt_gc_allocate(type, type->instance_size);
    thrown->message = message;
    thrown->cause = cause;
    return (KRef)thrown;
}

/* `Throwable(cause)`, the one-argument form whose operand is the CAUSE rather than the message.
   Kotlin fills the message from the cause — `cause?.toString()` — so the two one-argument
   constructors differ in more than which field they fill, and a caller cannot rewrite one as the
   other. The rendering happens BEFORE the allocation, so nothing half-built is live across it.

   The cause's `toString` is the program's to override, and one that raises ends the constructor
   call there, as it does in Kotlin: nothing is constructed, and the caller's check finds the
   program's exception rather than a throwable built around the `null` the rendering came back
   with. */
KRef kt_throwable_new_from_cause(const KType *type, KRef cause) {
    KRef message = cause == NULL ? NULL : kt_to_string(cause);
    if (kt_raised()) {
        return NULL;
    }
    return kt_throwable_new_with_cause(type, message, cause);
}

/* The message `Throwable(cause)` gives an object the GENERATOR builds rather than this runtime —
   a class of the program extending `Throwable`, whose fields the constructor writes in place. The
   rendering is Kotlin's `cause?.toString()`, and it lives here because `toString` does. */
KRef kt_throwable_message_of_cause(KRef cause) {
    return cause == NULL ? NULL : kt_to_string(cause);
}

KRef kt_throwable_message(KRef self) { return ((KThrowable *)self)->message; }

KRef kt_throwable_cause(KRef self) { return ((KThrowable *)self)->cause; }

/* Reading a `lateinit` property before anything assigned it. Kotlin's exception and Kotlin's
   wording; the guard is at the READ, which is where kotlinc puts it too, because the field being
   null is the only evidence there is. */
void kt_uninitialized_property(KRef name) {
    KRef message = kt_string_plus(kt_string_utf8("lateinit property ", 18), name);
    message = kt_string_plus(message, kt_string_utf8(" has not been initialized", 25));
    kt_throw(kt_throwable_new(&kt_type_uninitialized_property_access_exception, message));
}

/* Every string literal interned so far, in one growable list. The list is the ROOT that keeps them
   alive, rather than each literal's slot being a root of its own: the collector's registry of global
   roots then holds the program's declarations and not its text, one entry for all the literals
   however many there are. A slot need not be traced to stay valid, because the collector does not
   move what it keeps. */
static KRef kt_literals;

/* A string LITERAL, interned. Kotlin promises that equal literals are the same object — `"a" ===
   "a"` is true, and a function returning a literal answers the identical string every call — so a
   literal cannot allocate where it is written. `slot` is a static one per distinct text, filled on
   first use.

   The list's slot is registered as a root BEFORE the list is allocated rather than after: it is
   reachable from that moment, holding NULL until the list exists, and a collection triggered by
   this very allocation finds a root it can trace rather than one it has not been told about. The
   new string is a root in `text` across the allocation that adds it to the list. */
KRef kt_string_literal(const char *bytes, kt_int length, KRef *slot) {
    if (*slot == NULL) {
        if (kt_literals == NULL) {
            kt_gc_add_global_root((void **)&kt_literals);
            kt_literals = kt_mutable_list_with_capacity(0);
        }
        KRef text = kt_string_utf8(bytes, length);
        kt_mutable_list_add(kt_literals, text);
        *slot = text;
    }
    return *slot;
}

/* ---- kotlin.test ----------------------------------------------------------------------------

   An assertion compares and renders its operands with THEIR `equals` and `toString`, which a
   program overrides, and one of those may raise. The raise is what the caller has to see: Kotlin's
   assertion never gets as far as its own `AssertionError`. `kt_throw` records into one slot, so
   raising that error after the operand's would overwrite it, and a `catch` for the operand's
   exception would miss. Each assertion therefore asks `kt_raised` after every call that reaches the
   program and returns at once when one did. */

/* Kotlin's own wording, which is what a failing assertion has to report: the caller's message
   first when there is one, then what was expected and what arrived. */
static KRef kt_assert_prefix(KRef message) {
    if (message == NULL) {
        return kt_string_utf8("", 0);
    }
    return kt_string_plus(message, kt_string_utf8(". ", 2));
}

static void kt_assert_fail(KRef text) {
    kt_throw(kt_throwable_new(&kt_type_assertion_error, text));
}

/* `assertFailsWith<T> { … }` when the block did not throw what it had to. `was` is the exception
   actually caught, or NULL when the block completed.

   The class is named by its descriptor, so it reads `kotlin.IllegalStateException` where kotlin-test
   on the JVM reads `class java.lang.IllegalStateException` — the same difference every other
   report on this target already carries, since these are Kotlin's classes and not the JVM's. */
void kt_assert_failed_to_throw(KRef message, const KType *expected, KRef was) {
    KRef text = kt_string_plus(kt_assert_prefix(message),
                               kt_string_utf8("Expected an exception of class ", 31));
    text = kt_string_plus(text, kt_string_utf8(expected->name, (kt_int)expected->name_length));
    text = kt_string_plus(text, kt_string_utf8(" to be thrown, but was ", 23));
    KRef outcome = was == NULL ? kt_string_utf8("completed successfully.", 23) : kt_to_string(was);
    if (kt_raised()) {
        return;
    }
    kt_assert_fail(kt_string_plus(text, outcome));
}

/* The report `assertEquals` and `assertSame` share: `Expected <expected>, actual <actual>` and then
   `tail`, or NULL when rendering an operand raised. Built in pieces because rendering either
   operand may itself allocate, and the text so far has to stay reachable across that — the same
   reason `kt_list_to_string` is a loop over `kt_string_plus` rather than a render into one
   buffer. Each operand is rendered on its own before it joins the text, so a raise is seen before
   anything is built from what the rendering came back with. */
static KRef kt_assert_expected_actual(KRef expected, KRef actual, KRef message, KRef tail) {
    KRef text = kt_string_plus(kt_assert_prefix(message), kt_string_utf8("Expected <", 10));
    KRef rendered = kt_to_string(expected);
    if (kt_raised()) {
        return NULL;
    }
    text = kt_string_plus(text, rendered);
    text = kt_string_plus(text, kt_string_utf8(">, actual <", 11));
    rendered = kt_to_string(actual);
    if (kt_raised()) {
        return NULL;
    }
    text = kt_string_plus(text, rendered);
    return kt_string_plus(text, tail);
}

/* `actual == expected`: the ACTUAL operand's `equals`, which is the comparison kotlin-test's
   `DefaultAsserter` makes. */
void kt_assert_equals(KRef expected, KRef actual, KRef message) {
    kt_boolean equal = kt_equals(actual, expected);
    if (equal || kt_raised()) {
        return;
    }
    KRef text = kt_assert_expected_actual(expected, actual, message, kt_string_utf8(">.", 2));
    if (text == NULL) {
        return;
    }
    kt_assert_fail(text);
}

/* `assertSame`/`assertNotSame`: IDENTITY, which is the whole of what separates them from
   `assertEquals` — two strings with the same text are equal and are not the same object. Kotlin's
   own wording for each, `... is not same.` and `Expected not same as <x>.`, pinned against the
   reference toolchain by `assertion_wording`.

   Built in pieces for the reason `assertEquals` is: rendering either operand allocates, and the
   text so far has to stay reachable across that. */
void kt_assert_same(KRef expected, KRef actual, KRef message) {
    if (expected == actual) {
        return;
    }
    KRef text =
        kt_assert_expected_actual(expected, actual, message, kt_string_utf8("> is not same.", 14));
    if (text == NULL) {
        return;
    }
    kt_assert_fail(text);
}

void kt_assert_not_same(KRef illegal, KRef actual, KRef message) {
    if (illegal != actual) {
        return;
    }
    KRef text =
        kt_string_plus(kt_assert_prefix(message), kt_string_utf8("Expected not same as <", 22));
    KRef rendered = kt_to_string(actual);
    if (kt_raised()) {
        return;
    }
    text = kt_string_plus(text, rendered);
    kt_assert_fail(kt_string_plus(text, kt_string_utf8(">.", 2)));
}

/* `assertTrue`/`assertFalse`: the caller's message is the WHOLE report when there is one, and
   Kotlin's own text only stands in for a missing one -- kotlin-test passes `message ?: "Expected
   value to be true."` to its asserter, with no prefix joined on. */
void kt_assert_true(kt_boolean actual, KRef message) {
    if (actual) {
        return;
    }
    kt_assert_fail(message != NULL ? message : kt_string_utf8("Expected value to be true.", 26));
}

void kt_assert_false(kt_boolean actual, KRef message) {
    if (!actual) {
        return;
    }
    kt_assert_fail(message != NULL ? message : kt_string_utf8("Expected value to be false.", 27));
}

/* ---- callable references ----------------------------------------------------------------- */

/* The receiver a bound reference bound, or NULL for an unbound one. The descriptor says WHERE it
   is rather than this inferring it: a reference to a local function carries that function's
   ordinary captures as reference fields too, so the first of them is only the receiver when there
   are no captures — which is exactly the case this used to be restricted to. */
static KRef kt_reference_receiver(KRef self) {
    const KType *type = self->header.type;
    if (type->reference_receiver_offset == 0) {
        return NULL;
    }
    return *(KRef *)((uint8_t *)self + type->reference_receiver_offset);
}

kt_boolean kt_reference_equals(KRef self, KRef other) {
    if (other == NULL) {
        return false;
    }
    const void *mine = self->header.type->reference_target;
    /* An ordinary object's descriptor has none, so it is never equal to a reference — and neither
       is a reference to a DIFFERENT declaration, or a bound one to an unbound one. */
    if (mine == NULL || mine != other->header.type->reference_target) {
        return false;
    }
    return kt_equals(kt_reference_receiver(self), kt_reference_receiver(other));
}

kt_int kt_reference_hash_code(KRef self) {
    /* Equal references must hash alike, so the hash is built from exactly what equality reads. The
       arithmetic is on the unsigned ring, where it wraps as Kotlin's `Int` does: a code address
       times 31 leaves the signed range for nearly every address, and signed overflow is undefined
       in C, which would leave two equal references free to hash differently. */
    uint32_t hash = (uint32_t)(uintptr_t)self->header.type->reference_target;
    KRef receiver = kt_reference_receiver(self);
    if (receiver != NULL) {
        /* The receiver's `hashCode` is the program's, and one that raised answered nothing to
           combine: the caller's check propagates the raise and never reads this answer. */
        kt_int receiver_hash = kt_hash_code(receiver);
        if (kt_raised()) {
            return 0;
        }
        hash = 31u * hash + (uint32_t)receiver_hash;
    }
    return (kt_int)hash;
}

/* The exception in flight, or NULL.

   One slot, because one thread: a throw stores here and every call site that could observe it
   loads here. It is a GC root — the exception is unreachable from any frame between the throw and
   the `catch` that names it, and registering the slot is what keeps it alive across the
   allocations a `finally` or a handler's own code may perform along the way.

   `kt_pending_root` is what makes the registration happen once, from whichever entry point runs
   first, rather than needing a startup hook the generated program has to remember to call.

   The slot is EXPORTED because generated code reads it directly rather than through a call. That
   is not a micro-optimization: a call clobbers the caller-saved registers, so putting one after
   every call doubles what a frame must keep alive across a call boundary, and the frames grow.
   A 100,000-deep recursion in the corpus overflowed its stack on exactly that. A load and a
   perfectly predicted branch per call that can throw is the whole price of this design. */
KRef kt_pending;
static kt_boolean kt_pending_root;

static void kt_pending_register(void) {
    if (!kt_pending_root) {
        kt_pending_root = 1;
        kt_gc_add_global_root((void **)&kt_pending);
    }
}

/* `throw e`: record it and RETURN. The caller's next act is the check below, which is what turns
   the return into propagation. Not unwind tables, because this target has no unwinder: it would
   need emitted unwind information, a linker placing `.eh_frame` and a CFI interpreter in the
   runtime before a single `try` ran. Not `setjmp`, because it returns twice and the code generator
   cannot express that, so a value kept in a register across a `try` would be stale after the jump.
   A pending slot is ordinary control flow, identical on every architecture. */
void kt_throw(KRef thrown) {
    kt_pending_register();
    kt_pending = thrown;
}

/* The in-flight exception, for the check a call site makes and for the clause that matches it. */
KRef kt_pending_exception(void) { return kt_pending; }

/* A `catch` clause took it: nothing is in flight any more. */
void kt_clear_pending(void) { kt_pending = NULL; }

/* Nothing handled it. Kotlin ends the program, reporting the exception on stderr; 134 is the code
   this target uses for every abnormal end. Called once, where the generated entry has run the
   program's `main` and is about to treat its answer as an answer.

   The report renders the exception with its own `toString`, which the program may override, so the
   slot is EMPTIED first: generated code checks the slot after every call it makes, and a
   `toString` entered with the uncaught exception still there would take it for its own raise after
   its first call and come back with nothing. The exception stays reachable from `uncaught`, a
   local, across whatever the rendering allocates.

   A `toString` that raises leaves no text to print and no caller to propagate to. The JVM's
   answer, which this one copies, is the report's opening and then a line naming the class of the
   exception the `toString` raised; the class is read from its descriptor, which runs no program
   code. */
void kt_check_uncaught(void) {
    if (kt_pending == NULL) {
        return;
    }
    KRef uncaught = kt_pending;
    kt_clear_pending();
    kt_sys_write(2, "Exception in thread \"main\" ", 27);
    kt_int length = 0;
    KRef storage = NULL;
    const char *bytes = kt_render(uncaught, &length, &storage);
    if (kt_raised()) {
        const KType *raised = kt_pending->header.type;
        kt_sys_write(2, "\nException: ", 12);
        kt_sys_write(2, raised->name, raised->name_length);
        kt_sys_write(2, " thrown from the UncaughtExceptionHandler in thread \"main\"\n", 59);
        kt_sys_exit(134);
    }
    kt_sys_write(2, bytes, (size_t)length);
    kt_sys_write(2, "\n", 1);
    kt_sys_exit(134);
}

/* ---- kotlin.io ----------------------------------------------------------------------------- */

/* `print`/`println`. Kotlin renders the value before it writes a byte, and a `toString` of the
   program's that raises ends the call there: nothing is written, not even the `null` the renderer
   falls back to or the newline after it, and the caller's check propagates the raise. */
static void kt_emit(KRef value, bool newline) {
    kt_int length = 0;
    KRef storage = NULL;
    const char *bytes = kt_render(value, &length, &storage);
    if (kt_raised()) {
        return;
    }
    kt_sys_write(1, bytes, (size_t)length);
    if (newline) {
        kt_sys_write(1, "\n", 1);
    }
}

#define KT_CONSOLE(suffix, type, boxer)                                                            \
    void kt_print_##suffix(type value) { kt_emit(boxer(value), false); }                           \
    void kt_println_##suffix(type value) { kt_emit(boxer(value), true); }

static KRef kt_identity(KRef value) { return value; }

KT_CONSOLE(any, KRef, kt_identity)
KT_CONSOLE(byte, kt_byte, kt_box_byte)
KT_CONSOLE(short, kt_short, kt_box_short)
KT_CONSOLE(int, kt_int, kt_box_int)
KT_CONSOLE(long, kt_long, kt_box_long)
KT_CONSOLE(char, kt_char, kt_box_char)
KT_CONSOLE(boolean, kt_boolean, kt_box_boolean)
KT_CONSOLE(float, kt_float, kt_box_float)
KT_CONSOLE(double, kt_double, kt_box_double)
/* The unsigned four box through their OWN descriptor, which is what `kt_render` reads to print the
   value rather than the signed number sharing its bits. */
KT_CONSOLE(ubyte, kt_byte, kt_box_ubyte)
KT_CONSOLE(ushort, kt_short, kt_box_ushort)
KT_CONSOLE(uint, kt_int, kt_box_uint)
KT_CONSOLE(ulong, kt_long, kt_box_ulong)

#undef KT_CONSOLE

void kt_println_unit(void) { kt_sys_write(1, "\n", 1); }
