/* krusty native runtime: the class facilities — identity, equality and hashing of any value, what
   `is` and a cast ask, and the stdlib functions that throw.

   A translation unit of its own, so that `krusty_rt.c` does not grow past what one file should
   hold. It shares the object layout and the string helpers through `krusty_internal.h`. */
#include "krusty_internal.h"

/* ---- classes ------------------------------------------------------------------------------- */

kt_boolean kt_any_equals(KRef self, KRef other) { return self == other; }

/* Derived from the address. The collector never moves an object (conservative roots forbid it;
   see krusty_gc.c), so an object's address is stable for its whole life and is a legitimate
   identity hash. The shifts fold the aligned low bits and the high bits into the 32 that count. */
kt_int kt_any_hash_code(KRef self) {
    uintptr_t address = (uintptr_t)self;
    return (kt_int)(uint32_t)((address >> 4) ^ (address >> 36));
}

/* `<qualified name>@<hex hashCode>`, Kotlin's default shape. The hash is asked through the
   object's own `hashCode`, as `Any.toString` asks it, so a class that overrides only `hashCode`
   reaches its override here -- and one that throws ends the rendering with NULL and the exception
   pending, before the text is built. */
KRef kt_any_to_string(KRef self) {
    const KType *type = self->header.type;
    uint32_t hash = (uint32_t)kt_hash_code(self);
    if (kt_pending_exception() != NULL) {
        return NULL;
    }
    char digits[8];
    kt_int digit_count = 0;
    do {
        uint32_t nibble = hash & 0xFu;
        digits[digit_count++] = (char)(nibble < 10 ? '0' + nibble : 'a' + (nibble - 10));
        hash >>= 4;
    } while (hash != 0);
    kt_int length = (kt_int)type->name_length + 1 + digit_count;
    KByteArray *buffer = kt_bytes_new(length);
    char *out = kt_bytes_of(buffer);
    memcpy(out, type->name, type->name_length);
    out[type->name_length] = '@';
    for (kt_int i = 0; i < digit_count; i++) {
        out[type->name_length + 1 + i] = digits[digit_count - 1 - i];
    }
    return kt_string_of((KRef)buffer, out, length);
}

KRef kt_object_to_string(KRef value) {
    const KType *type = value->header.type;
    /* A type with no vtable (a hand-written test type) still renders as kotlin.Any would. */
    if (type->vtable == NULL || type->vtable_length <= KT_SLOT_TO_STRING) {
        return kt_any_to_string(value);
    }
    return ((KRef(*)(KRef))type->vtable[KT_SLOT_TO_STRING])(value);
}

kt_boolean kt_equals(KRef a, KRef b) {
    if (a == NULL) {
        return b == NULL;
    }
    const KType *type = a->header.type;
    if (type->vtable == NULL) {
        return a == b;
    }
    return ((kt_boolean(*)(KRef, KRef))type->vtable[KT_SLOT_EQUALS])(a, b);
}

kt_int kt_hash_code(KRef value) {
    if (value == NULL) {
        return 0;
    }
    const KType *type = value->header.type;
    if (type->vtable == NULL) {
        return kt_any_hash_code(value);
    }
    return ((kt_int(*)(KRef))type->vtable[KT_SLOT_HASH_CODE])(value);
}

/* The bits `equals` and `hashCode` read from a floating-point value: every NaN collapsed to ONE.

   This is `java.lang.Double.doubleToLongBits`, and the difference from `doubleToRawLongBits` is
   the whole point. `0.0 / 0.0` produces a NaN with the sign bit SET on x86 (`fff8…`) where the
   `Double.NaN` constant does not (`7ff8…`), so comparing raw bits answers false for two values
   Kotlin calls equal — and hashes them differently, which would break the contract between them.
   Kotlin has ONE NaN as far as `equals` is concerned, and this is where that is decided. */
uint64_t kt_double_bits(kt_double value) {
    uint64_t bits;
    memcpy(&bits, &value, sizeof bits);
    if ((bits & 0x7FF0000000000000ULL) == 0x7FF0000000000000ULL &&
        (bits & 0x000FFFFFFFFFFFFFULL) != 0) {
        return 0x7FF8000000000000ULL;
    }
    return bits;
}

uint32_t kt_float_bits(kt_float value) {
    uint32_t bits;
    memcpy(&bits, &value, sizeof bits);
    if ((bits & 0x7F800000u) == 0x7F800000u && (bits & 0x007FFFFFu) != 0) {
        return 0x7FC00000u;
    }
    return bits;
}

/* Built-in values compare by value, as Kotlin's `==` on boxed values does: two `Int?` holding 3
   are equal, and two strings with the same text are equal. */
kt_boolean kt_builtin_equals(KRef self, KRef other) {
    if (self == other) {
        return true;
    }
    if (other == NULL || self->header.type != other->header.type) {
        return false;
    }
    const KType *type = self->header.type;
    if (type == &kt_type_string) {
        if (self->as.string.byte_length != other->as.string.byte_length) {
            return false;
        }
        for (kt_int i = 0; i < self->as.string.byte_length; i++) {
            if (self->as.string.bytes[i] != other->as.string.bytes[i]) {
                return false;
            }
        }
        return true;
    }
    if (type == &kt_type_boolean) {
        return self->as.boolean_value == other->as.boolean_value;
    }
    if (type == &kt_type_char) {
        return self->as.char_value == other->as.char_value;
    }
    /* An unsigned value is stored in the signed field of its width, and the descriptors were
       already required to match above — so equality is the same bit comparison, and reading the
       bits as a value rather than as a sign cannot change its answer. */
    if (type == &kt_type_byte || type == &kt_type_ubyte) {
        return self->as.byte_value == other->as.byte_value;
    }
    if (type == &kt_type_short || type == &kt_type_ushort) {
        return self->as.short_value == other->as.short_value;
    }
    if (type == &kt_type_int || type == &kt_type_uint) {
        return self->as.int_value == other->as.int_value;
    }
    if (type == &kt_type_long || type == &kt_type_ulong) {
        return self->as.long_value == other->as.long_value;
    }
    /* Boxed floating-point values compare by BITS, which is what `equals` means in Kotlin and not
       what `==` on two `Double`s means: `Double.NaN.equals(Double.NaN)` is true where
       `Double.NaN == Double.NaN` is false, and `0.0.equals(-0.0)` is false where `0.0 == -0.0` is
       true. The scalar comparison the generator emits for `==` is the other rule, and neither is
       this one. */
    if (type == &kt_type_double) {
        return kt_double_bits(self->as.double_value) == kt_double_bits(other->as.double_value);
    }
    if (type == &kt_type_float) {
        return kt_float_bits(self->as.float_value) == kt_float_bits(other->as.float_value);
    }
    /* kotlin.Unit: one instance, already handled by identity above. */
    return false;
}

/* Kotlin's `hashCode` for the built-in values. A string hashes over its UTF-16 code units, as
   Kotlin specifies, which the UTF-8 text is decoded into on the way. */
kt_int kt_builtin_hash_code(KRef self) {
    const KType *type = self->header.type;
    if (type == &kt_type_string) {
        uint32_t hash = 0;
        const unsigned char *bytes = (const unsigned char *)self->as.string.bytes;
        kt_int length = self->as.string.byte_length;
        kt_int at = 0;
        while (at < length) {
            uint32_t lead = bytes[at];
            uint32_t code_point;
            kt_int width;
            if (lead < 0x80) {
                code_point = lead;
                width = 1;
            } else if (lead < 0xE0) {
                code_point = lead & 0x1F;
                width = 2;
            } else if (lead < 0xF0) {
                code_point = lead & 0x0F;
                width = 3;
            } else {
                code_point = lead & 0x07;
                width = 4;
            }
            for (kt_int i = 1; i < width && at + i < length; i++) {
                code_point = (code_point << 6) | (bytes[at + i] & 0x3Fu);
            }
            at += width;
            if (code_point >= 0x10000) {
                uint32_t offset = code_point - 0x10000;
                hash = 31u * hash + (0xD800u + (offset >> 10));
                hash = 31u * hash + (0xDC00u + (offset & 0x3FFu));
            } else {
                hash = 31u * hash + code_point;
            }
        }
        return (kt_int)hash;
    }
    if (type == &kt_type_boolean) {
        return self->as.boolean_value ? 1231 : 1237;
    }
    if (type == &kt_type_char) {
        return (kt_int)self->as.char_value;
    }
    /* Kotlin defines each unsigned `hashCode` as the wrapped signed value's, so the grouping is
       the specification and not a shortcut: `(-1).hashCode()` and `4294967295u.hashCode()` are
       the same number. */
    if (type == &kt_type_byte || type == &kt_type_ubyte) {
        return (kt_int)self->as.byte_value;
    }
    if (type == &kt_type_short || type == &kt_type_ushort) {
        return (kt_int)self->as.short_value;
    }
    if (type == &kt_type_int || type == &kt_type_uint) {
        return self->as.int_value;
    }
    if (type == &kt_type_long || type == &kt_type_ulong) {
        uint64_t bits = (uint64_t)self->as.long_value;
        return (kt_int)(uint32_t)(bits ^ (bits >> 32));
    }
    /* Kotlin's answers for these are fixed — a program can print a hash — and they are the bits,
       folded for a `Double` the way a `Long`'s are. */
    if (type == &kt_type_double) {
        /* Through the same canonicalization `equals` uses: two values that compare equal must hash
           equal, and two NaNs do compare equal. */
        uint64_t bits = kt_double_bits(self->as.double_value);
        return (kt_int)(uint32_t)(bits ^ (bits >> 32));
    }
    if (type == &kt_type_float) {
        return (kt_int)kt_float_bits(self->as.float_value);
    }
    return kt_any_hash_code(self);
}

kt_boolean kt_is_instance(KRef object, const KType *type) {
    if (object == NULL) {
        return false;
    }
    for (const KType *at = object->header.type; at != NULL; at = at->super) {
        if (at == type) {
            return true;
        }
        /* An interface is not on the super chain, so each type carries the ones it implements.
           The list is already transitive, so this is a scan and not a second walk. */
        for (uint32_t i = 0; i < at->interface_count; i++) {
            if (at->interfaces[i] == type) {
                return true;
            }
        }
    }
    return false;
}

/* A failed cast is `ClassCastException`, and a program is entitled to catch it. The wording is
   Kotlin/Native's — `class A cannot be cast to class B`, both sides qualified — which is what the
   corpus's `nativeCCEMessage` cases read.

   Every intermediate stays in a local across the allocations that follow it, so the collector sees
   each as a root while the next piece is built. */
static void kt_fail_cast(KRef object, const KType *type) {
    KRef from = object == NULL
                    ? kt_string_utf8("null", 4)
                    : kt_string_utf8(object->header.type->name, object->header.type->name_length);
    KRef message = kt_string_plus(kt_string_utf8("class ", 6), from);
    message = kt_string_plus(message, kt_string_utf8(" cannot be cast to class ", 25));
    message = kt_string_plus(message, kt_string_utf8(type->name, type->name_length));
    kt_throw(kt_throwable_new(&kt_type_class_cast_exception, message));
}

/* `a.compareTo(b)` where the static type says only `Comparable`.

   The DESCRIPTOR says what to compare, exactly as `kt_equals` and `kt_to_string` read it, and only
   the orders the RUNTIME defines are here: a boxed primitive at its own width — with Kotlin's total
   order for the floating ones, where -0.0 sits below 0.0 and every NaN above everything — a string
   by UTF-16 unit, and the unsigned integers read unsigned. A program's own `Comparable` is not
   among them: an object of the program's could stand behind that type too and no static type tells
   the two apart, so a file that declares one declines at the CALL SITE, where the type it named is
   still in sight.

   Each side is unboxed at its OWN descriptor's field, not through one reader: the boxes share a
   union, so reading a `Byte`'s payload as an `Int` reads bytes that were never written.

   Two values of different types have no order between them, which is what `Comparable<Any>` runs
   into. The JVM raises `ClassCastException` there and so does this; `kt_throw` records it and comes
   back, so the raise is followed by a return. */
kt_int kt_compare_any(KRef a, KRef b) {
    if (a == NULL || b == NULL) {
        kt_null_receiver();
        return 0;
    }
    const KType *type = a->header.type;
    if (type != b->header.type) {
        kt_fail_cast(b, type);
        return 0;
    }
    if (type == &kt_type_string) {
        return kt_string_compare_to(a, b);
    }
    if (type == &kt_type_byte) {
        return kt_compare_byte(kt_unbox_byte(a), kt_unbox_byte(b));
    }
    if (type == &kt_type_short) {
        return kt_compare_short(kt_unbox_short(a), kt_unbox_short(b));
    }
    if (type == &kt_type_int) {
        return kt_compare_int(kt_unbox_int(a), kt_unbox_int(b));
    }
    if (type == &kt_type_long) {
        return kt_compare_long(kt_unbox_long(a), kt_unbox_long(b));
    }
    if (type == &kt_type_char) {
        return kt_compare_char(kt_unbox_char(a), kt_unbox_char(b));
    }
    if (type == &kt_type_boolean) {
        return kt_compare_boolean(kt_unbox_boolean(a), kt_unbox_boolean(b));
    }
    if (type == &kt_type_float) {
        return kt_compare_float(kt_unbox_float(a), kt_unbox_float(b));
    }
    if (type == &kt_type_double) {
        return kt_compare_double(kt_unbox_double(a), kt_unbox_double(b));
    }
    /* The unsigned four. Each box holds the signed type's bits, so the comparison is the one the
       widths share once both sides are read as unsigned. */
    if (type == &kt_type_ubyte || type == &kt_type_ushort || type == &kt_type_uint) {
        uint32_t left = type == &kt_type_ubyte    ? (uint8_t)kt_unbox_ubyte(a)
                        : type == &kt_type_ushort ? (uint16_t)kt_unbox_ushort(a)
                                                  : (uint32_t)kt_unbox_uint(a);
        uint32_t right = type == &kt_type_ubyte    ? (uint8_t)kt_unbox_ubyte(b)
                         : type == &kt_type_ushort ? (uint16_t)kt_unbox_ushort(b)
                                                   : (uint32_t)kt_unbox_uint(b);
        return left < right ? -1 : (left > right ? 1 : 0);
    }
    if (type == &kt_type_ulong) {
        uint64_t left = (uint64_t)kt_unbox_ulong(a);
        uint64_t right = (uint64_t)kt_unbox_ulong(b);
        return left < right ? -1 : (left > right ? 1 : 0);
    }
    KT_FAIL("krusty: a comparison of a type the runtime has no order for\n");
    return 0;
}

KRef kt_cast(KRef object, const KType *type) {
    if (object != NULL && !kt_is_instance(object, type)) {
        kt_fail_cast(object, type);
    }
    return object;
}

/* `null as String` is a NullPointerException NAMING the target type, not a ClassCastException —
   `null` is not an instance of anything, so there is no class to report as the source. kotlinc's
   exact wording, and its exact type. */
static void kt_fail_null_cast(const char *target, kt_int target_length) {
    KRef message = kt_string_plus(kt_string_utf8("null cannot be cast to non-null type ", 37),
                                  kt_string_utf8(target, target_length));
    kt_throw(kt_throwable_new(&kt_type_null_pointer_exception, message));
}

KRef kt_cast_non_null(KRef object, const KType *type) {
    if (object == NULL) {
        kt_fail_null_cast(type->name, type->name_length);
        return object;
    }
    if (!kt_is_instance(object, type)) {
        kt_fail_cast(object, type);
    }
    return object;
}

KRef kt_safe_cast(KRef object, const KType *type) {
    return kt_is_instance(object, type) ? object : NULL;
}

/* A non-null cast whose target has no descriptor to test against, an erased type parameter: only
   the null is checked, and the message names the target as the generator rendered it. */
KRef kt_cast_non_null_erased(KRef object, const char *target, kt_int target_length) {
    if (object == NULL) {
        kt_fail_null_cast(target, target_length);
    }
    return object;
}

/* `x!!` on a null: Kotlin's `NullPointerException`, with NO message — which is what kotlinc emits
   and is observably different from the null CAST below, whose message names the target type. */
KRef kt_not_null(KRef value) {
    if (value == NULL) {
        kt_throw(kt_throwable_new(&kt_type_null_pointer_exception, NULL));
    }
    return value;
}

/* The stdlib functions that throw. Each builds the exception Kotlin specifies, with Kotlin's own
   message, and hands it to `kt_throw` — the same path a `throw` the program wrote itself takes.
   That matters beyond tidiness: `error(m)` and `throw IllegalStateException(m)` are the same
   exception in Kotlin, so a `catch` must not be able to tell them apart, and the surest way to
   keep that true is for there to be only one object and one report.

   These report the exception rather than a `krusty:` line of their own, which they did while there
   was no `Throwable` to report. */
#define KT_THROW(type, message) kt_throw(kt_throwable_new(&(type), message))

/* A literal Kotlin message. */
#define KT_MESSAGE(text) kt_string_utf8(text, (kt_int)(sizeof(text) - 1))

void kt_not_implemented(void) {
    KT_THROW(kt_type_not_implemented_error, KT_MESSAGE("An operation is not implemented."));
}

/* A message whose `toString` THROWS raises that exception instead of the thrower's own, as
   Kotlin's does: the message is built before the exception that carries it, so what the rendering
   raised is what propagates. `kt_throw` would overwrite it, so the thrower returns first. */
void kt_not_implemented_reason(KRef reason) {
    KRef text = kt_to_string(reason);
    if (kt_pending_exception() != NULL) {
        return;
    }
    KT_THROW(kt_type_not_implemented_error,
             kt_string_plus(KT_MESSAGE("An operation is not implemented: "), text));
}

/* `error(message)` takes an `Any`, and the exception carries its `toString` -- or, when that
   throws, the exception is the one it threw, as above. */
void kt_illegal_state(KRef message) {
    KRef text = kt_to_string(message);
    if (kt_pending_exception() != NULL) {
        return;
    }
    KT_THROW(kt_type_illegal_state_exception, text);
}

void kt_require(kt_boolean value) {
    if (!value) {
        KT_THROW(kt_type_illegal_argument_exception, KT_MESSAGE("Failed requirement."));
    }
}

/* The overflow guard `forEachIndexed` and its relatives carry, spliced into a caller by an inline
   stdlib body. Kotlin's own wording. */
void kt_throw_index_overflow(void) {
    KT_THROW(kt_type_arithmetic_exception, KT_MESSAGE("Index overflow has happened."));
}

/* `Property x should be initialized before get.` — Kotlin's own text for a `notNull` delegate read
   before it was written. */
void kt_raise_uninitialized_property(KRef name) {
    KRef message = kt_string_plus(KT_MESSAGE("Property "), kt_to_string(name));
    message = kt_string_plus(message, KT_MESSAGE(" should be initialized before get."));
    KT_THROW(kt_type_illegal_state_exception, message);
}

/* Kotlin's `assert(value)` and `assert(value) { message }`, on the failing side. The generator
   branches on the condition and reaches this only when it is false, which is what keeps the message
   from being computed on the passing path — Kotlin's `lazyMessage` is lazy exactly there.

   The message arrives as the FUNCTION rather than as text, and is invoked here through the one slot
   every function value declares, as `kt_lazy_value` invokes an initializer. NULL is the form that
   wrote no message, whose text Kotlin fixes as "Assertion failed". */
void kt_assertion_failed(KRef lazy_message) {
    if (lazy_message == NULL) {
        KT_THROW(kt_type_assertion_error, KT_MESSAGE("Assertion failed"));
        return;
    }
    if (lazy_message->header.type->vtable == NULL ||
        lazy_message->header.type->vtable_length <= KT_SLOT_INVOKE) {
        KT_FAIL("krusty: an assertion message is not a function value\n");
    }
    /* A message lambda that THROWS -- `assert(false) { error("boom") }` -- propagates what it
       threw, and so does a message whose `toString` throws: Kotlin computes the message before it
       constructs the `AssertionError`, so it never gets that far. `kt_throw` overwrites the pending
       slot, so each step returns rather than raising over it. */
    KRef message =
        ((KRef(*)(KRef))lazy_message->header.type->vtable[KT_SLOT_INVOKE])(lazy_message);
    if (kt_pending_exception() != NULL) {
        return;
    }
    KRef text = kt_to_string(message);
    if (kt_pending_exception() != NULL) {
        return;
    }
    KT_THROW(kt_type_assertion_error, text);
}

void kt_check(kt_boolean value) {
    if (!value) {
        KT_THROW(kt_type_illegal_state_exception, KT_MESSAGE("Check failed."));
    }
}

#undef KT_MESSAGE
#undef KT_THROW

void kt_abstract_method_called(void) { KT_FAIL("krusty: abstract method called\n"); }

/* A member access on `null`: Kotlin's `NullPointerException`, with no message, which a program may
   catch. Kotlin/Native raises exactly that; the JVM's message, when it has one, describes the Java
   call that failed, which a native program has none of. */
void kt_null_receiver(void) { kt_throw(kt_throwable_new(&kt_type_null_pointer_exception, NULL)); }

/* A callable whose Kotlin type is `Nothing` returned instead of diverging, and the path that reads
   its value has no value to read. The JVM throws `KotlinNothingValueException` here. This one is
   NOT raised as a `Throwable`, unlike the stdlib's own throwers: reaching it means a callee lied
   about its type, which is a defect in what was emitted rather than something a program is entitled
   to catch, so it stays the loud, uncatchable failure a failed cast is. */
void kt_nothing_value_returned(void) {
    KT_FAIL("krusty: a `Nothing`-typed callable returned a value\n");
}
