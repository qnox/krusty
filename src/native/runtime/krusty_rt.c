/* krusty native runtime — generated; do not edit. */
#include "krusty_rt.h"
#include "krusty_sys.h"

/* ---- kernel interface ---------------------------------------------------------------------- */

void kt_exit(kt_int status) { kt_sys_exit(status); }

static void kt_write(kt_int fd, const char *bytes, size_t length) {
    kt_sys_write(fd, bytes, length);
}

#define KT_FAIL(literal) KT_SYS_FAIL(literal)

/* ---- freestanding C support --------------------------------------------------------------- */

/* A compiler may synthesize calls to these from ordinary assignments and loops even under
   -ffreestanding, so they have to exist as real symbols. */
void *memcpy(void *destination, const void *source, size_t length) {
    unsigned char *out = (unsigned char *)destination;
    const unsigned char *in = (const unsigned char *)source;
    for (size_t index = 0; index < length; index++) {
        out[index] = in[index];
    }
    return destination;
}

void *memset(void *destination, int value, size_t length) {
    unsigned char *out = (unsigned char *)destination;
    for (size_t index = 0; index < length; index++) {
        out[index] = (unsigned char)value;
    }
    return destination;
}

/* ---- object model -------------------------------------------------------------------------- */

static kt_boolean kt_builtin_equals(KRef self, KRef other);
static kt_int kt_builtin_hash_code(KRef self);

/* Every built-in value type shares one vtable: value equality, Kotlin's hash for that value, and
   the runtime's own rendering as toString. */
static const kt_fn kt_builtin_vtable[] = {(kt_fn)kt_builtin_equals, (kt_fn)kt_builtin_hash_code,
                                          (kt_fn)kt_to_string};

static const kt_fn kt_any_vtable[] = {(kt_fn)kt_any_equals, (kt_fn)kt_any_hash_code,
                                      (kt_fn)kt_any_to_string};

/* kotlin.Any itself is never instantiated; the descriptor exists as the root of every `super`
   chain and the owner of the three default slots. */
const KType kt_type_any = {"kotlin.Any", 10,   sizeof(KObjectHeader), 0, 0, NULL, NULL,
                           kt_any_vtable, 3, 0};

/* `kotlin.Number` and `kotlin.Comparable` have no instances of their OWN — every value that is one
   is a boxed primitive or a string. They exist as descriptors for those to point at, so that an `is`
   against them has something to compare. Kotlin's own hierarchy decides which points at which, and
   it is asymmetric: `Char` and `Boolean` are `Comparable` and not `Number`, and an unsigned integer
   is `Comparable` and not `Number` either (it is a value class, not a `java.lang.Number`). */
const KType kt_type_number = {"kotlin.Number", 13,  sizeof(KObjectHeader), 0, 0, NULL, &kt_type_any,
                              kt_any_vtable,   3,   0};
const KType kt_type_comparable = {"kotlin.Comparable", 17, sizeof(KObjectHeader), 0, 0, NULL,
                                  &kt_type_any,        kt_any_vtable, 3, 0};

/* `kotlin.CharSequence` is the same kind of thing: no instances of its own, and TWO types point at
   it — a `String` and a `StringBuilder`. Both are needed. Answering this question with
   `kt_type_string` would have been sound while a string was the only text this runtime made, and
   stopped being sound the moment there was a builder. */
const KType kt_type_char_sequence = {"kotlin.CharSequence", 19, sizeof(KObjectHeader), 0, 0, NULL,
                                     &kt_type_any,         kt_any_vtable, 3, 0};

/* `kotlin.Function` and each arity of it. A function value is an object of a type of its own —
   the generator emits one per lambda and per callable reference — so an `is` against a function
   type has no single descriptor to compare against. These are that descriptor: each function
   value's own type names its arity's marker and the bare `Function` beside it.

   None of them has instances, exactly like `Number` and `Comparable` above. 22 is Kotlin's largest
   function arity, so the set is complete rather than open-ended. */
const KType kt_type_function = {"kotlin.Function", 15,            sizeof(KObjectHeader), 0, 0, NULL,
                                &kt_type_any,      kt_any_vtable, 3,                     0};

#define KT_FUNCTION_TYPE(arity)                                                                    \
    const KType kt_type_function##arity = {"kotlin.Function" #arity,                               \
                                           sizeof("kotlin.Function" #arity) - 1,                   \
                                           sizeof(KObjectHeader),                                  \
                                           0,                                                      \
                                           0,                                                      \
                                           NULL,                                                   \
                                           &kt_type_any,                                           \
                                           kt_any_vtable,                                          \
                                           3,                                                      \
                                           0};

KT_FUNCTION_TYPE(0)
KT_FUNCTION_TYPE(1)
KT_FUNCTION_TYPE(2)
KT_FUNCTION_TYPE(3)
KT_FUNCTION_TYPE(4)
KT_FUNCTION_TYPE(5)
KT_FUNCTION_TYPE(6)
KT_FUNCTION_TYPE(7)
KT_FUNCTION_TYPE(8)
KT_FUNCTION_TYPE(9)
KT_FUNCTION_TYPE(10)
KT_FUNCTION_TYPE(11)
KT_FUNCTION_TYPE(12)
KT_FUNCTION_TYPE(13)
KT_FUNCTION_TYPE(14)
KT_FUNCTION_TYPE(15)
KT_FUNCTION_TYPE(16)
KT_FUNCTION_TYPE(17)
KT_FUNCTION_TYPE(18)
KT_FUNCTION_TYPE(19)
KT_FUNCTION_TYPE(20)
KT_FUNCTION_TYPE(21)
KT_FUNCTION_TYPE(22)

/* Kotlin's reflection hierarchy, as far as a PROPERTY REFERENCE wears it. None has instances of
   its own — a reference object's type is one the generator emits per property — so these are what
   an `is` against `KProperty0` or `KMutableProperty` compares with. */
#define KT_REFLECT_TYPE(identifier, kotlin_name)                                                   \
    const KType identifier = {kotlin_name,       sizeof(kotlin_name) - 1,                          \
                              sizeof(KObjectHeader), 0,                                            \
                              0,                 NULL,                                             \
                              &kt_type_any,      kt_any_vtable,                                    \
                              3,                 0};

KT_REFLECT_TYPE(kt_type_kcallable, "kotlin.reflect.KCallable")
KT_REFLECT_TYPE(kt_type_kproperty, "kotlin.reflect.KProperty")
KT_REFLECT_TYPE(kt_type_kproperty0, "kotlin.reflect.KProperty0")
KT_REFLECT_TYPE(kt_type_kproperty1, "kotlin.reflect.KProperty1")
KT_REFLECT_TYPE(kt_type_kproperty2, "kotlin.reflect.KProperty2")
KT_REFLECT_TYPE(kt_type_kmutable_property, "kotlin.reflect.KMutableProperty")
KT_REFLECT_TYPE(kt_type_kmutable_property0, "kotlin.reflect.KMutableProperty0")
KT_REFLECT_TYPE(kt_type_kmutable_property1, "kotlin.reflect.KMutableProperty1")
KT_REFLECT_TYPE(kt_type_kmutable_property2, "kotlin.reflect.KMutableProperty2")

/* Flattened and transitive, as `KType.interfaces` requires: a `Number` is also `Comparable`, so a
   numeric box names both rather than relying on a walk that does not exist. */
static const KType *const kt_number_interfaces[] = {&kt_type_number, &kt_type_comparable};
static const KType *const kt_comparable_interfaces[] = {&kt_type_comparable};
static const KType *const kt_text_interfaces[] = {&kt_type_comparable, &kt_type_char_sequence};
static const KType *const kt_char_sequence_interfaces[] = {&kt_type_char_sequence};

#define KT_TYPE_WITH(identifier, kotlin_name, size, count, offsets, ifaces)                        \
    const KType identifier = {kotlin_name,       sizeof(kotlin_name) - 1,                          \
                              size,              count,                                            \
                              0,                 offsets,                                          \
                              &kt_type_any,      kt_builtin_vtable,                                \
                              3,                 0,                                                \
                              ifaces,            (uint32_t)(sizeof(ifaces) / sizeof((ifaces)[0]))};

#define KT_TYPE(identifier, kotlin_name, size, count, offsets)                                     \
    const KType identifier = {kotlin_name, sizeof(kotlin_name) - 1, size,  count,                  \
                              0,           offsets,                 &kt_type_any,                  \
                              kt_builtin_vtable, 3, 0};

/* An array type. Its members are compared by IDENTITY, which is what Kotlin's `==` on arrays means,
   so it takes `kotlin.Any`'s vtable rather than the built-in value one. */
#define KT_ARRAY_TYPE(identifier, kotlin_name, stride, references)                                 \
    const KType identifier = {kotlin_name, sizeof(kotlin_name) - 1, sizeof(KArray), 0,             \
                              stride,      NULL,                    &kt_type_any,                  \
                              kt_any_vtable, 3, references};

/* Every array, including the raw bytes behind a string's text. `KArray` says where the elements
   begin; the type says how wide they are and whether the collector looks inside. */
KT_ARRAY_TYPE(kt_type_array, "kotlin.Array", sizeof(void *), 1)
KT_ARRAY_TYPE(kt_type_byte_array, "kotlin.ByteArray", 1, 0)
KT_ARRAY_TYPE(kt_type_short_array, "kotlin.ShortArray", 2, 0)
KT_ARRAY_TYPE(kt_type_int_array, "kotlin.IntArray", 4, 0)
KT_ARRAY_TYPE(kt_type_long_array, "kotlin.LongArray", 8, 0)
KT_ARRAY_TYPE(kt_type_char_array, "kotlin.CharArray", 2, 0)
KT_ARRAY_TYPE(kt_type_boolean_array, "kotlin.BooleanArray", 1, 0)
KT_ARRAY_TYPE(kt_type_float_array, "kotlin.FloatArray", 4, 0)
KT_ARRAY_TYPE(kt_type_double_array, "kotlin.DoubleArray", 8, 0)

/* An unsigned array is a value class over the signed array of the same width, so it takes that
   array's STRIDE and its own NAME. Sharing the signed descriptor would read and write the same
   bytes correctly and answer `is IntArray` with `true`, where the two are distinct classes. */
KT_ARRAY_TYPE(kt_type_ubyte_array, "kotlin.UByteArray", 1, 0)
KT_ARRAY_TYPE(kt_type_ushort_array, "kotlin.UShortArray", 2, 0)
KT_ARRAY_TYPE(kt_type_uint_array, "kotlin.UIntArray", 4, 0)
KT_ARRAY_TYPE(kt_type_ulong_array, "kotlin.ULongArray", 8, 0)

KRef kt_array_new(const KType *type, kt_int length) {
    if (length < 0) {
        KT_FAIL("krusty: negative array size\n");
    }
    KArray *array = (KArray *)kt_gc_allocate(
        type, (uint32_t)sizeof(KArray) + (uint32_t)length * type->element_size);
    array->length = length;
    return (KRef)array;
}

/* An index outside `0 until size`: Kotlin's `IndexOutOfBoundsException`, which a program may
   catch. The wording is the JVM's, which is what the corpus reads where it reads one at all. */
void kt_index_out_of_bounds(kt_int index, kt_int size) {
    KRef message = kt_string_plus(kt_string_utf8("Index ", 6), kt_to_string(kt_box_int(index)));
    message = kt_string_plus(message, kt_string_utf8(" out of bounds for length ", 26));
    message = kt_string_plus(message, kt_to_string(kt_box_int(size)));
    kt_throw(kt_throwable_new(&kt_type_index_out_of_bounds_exception, message));
}

/* `kotlin.Enum`'s own storage, which every enum class carries ahead of its own fields. The
   generator writes both when it builds a constant; the layout is here because the base class is
   the language's, not any file's. */
typedef struct KEnum {
    KObjectHeader header;
    KRef name;
    kt_int ordinal;
} KEnum;

/* Kotlin's `Enum.toString()` is the constant's name, and `kotlin.Any`'s identity rendering is not.
   This sits in the `toString` slot of every enum class's table. */
KRef kt_enum_to_string(KRef self) { return ((KEnum *)self)->name; }

/* An exhaustive `when` used as a value has no `else` to fall into. Kotlin's own answer for the
   case its exhaustiveness check missed is `NoWhenBranchMatchedException`; without exceptions, this
   is the same statement, made loudly. */
void kt_no_when_branch_matched(void) {
    KT_FAIL("krusty: no branch of an exhaustive `when` matched\n");
}

/* `Color.valueOf("NOPE")` — Kotlin's `IllegalArgumentException`, naming the constant asked for. */
void kt_no_such_enum_constant(KRef name) {
    KRef message = kt_string_plus(kt_string_utf8("No enum constant ", 17), name);
    kt_throw(kt_throwable_new(&kt_type_illegal_argument_exception, message));
}

typedef KArray KByteArray;

static kt_int kt_length_of(KRef array) { return ((const KArray *)array)->length; }

static char *kt_bytes_of(KByteArray *array) { return (char *)(array + 1); }

static KByteArray *kt_bytes_new(kt_int length) {
    return (KByteArray *)kt_array_new(&kt_type_byte_array, length);
}

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

static const uint32_t kt_string_references[] = {offsetof(KObject, as.string.storage)};

KT_TYPE_WITH(kt_type_string, "kotlin.String", sizeof(KObject), 1, kt_string_references, kt_text_interfaces)
KT_TYPE_WITH(kt_type_byte, "kotlin.Byte", sizeof(KObject), 0, NULL, kt_number_interfaces)
KT_TYPE_WITH(kt_type_short, "kotlin.Short", sizeof(KObject), 0, NULL, kt_number_interfaces)
KT_TYPE_WITH(kt_type_int, "kotlin.Int", sizeof(KObject), 0, NULL, kt_number_interfaces)
KT_TYPE_WITH(kt_type_long, "kotlin.Long", sizeof(KObject), 0, NULL, kt_number_interfaces)
KT_TYPE_WITH(kt_type_char, "kotlin.Char", sizeof(KObject), 0, NULL, kt_comparable_interfaces)
KT_TYPE_WITH(kt_type_boolean, "kotlin.Boolean", sizeof(KObject), 0, NULL, kt_comparable_interfaces)
KT_TYPE_WITH(kt_type_float, "kotlin.Float", sizeof(KObject), 0, NULL, kt_number_interfaces)
KT_TYPE_WITH(kt_type_double, "kotlin.Double", sizeof(KObject), 0, NULL, kt_number_interfaces)
KT_TYPE(kt_type_unit, "kotlin.Unit", sizeof(KObject), 0, NULL)

/* Kotlin's four unsigned integers. Each is a value class over a signed primitive, and the generated
   code carries it as the machine integer it wraps — the right machine shape, and the wrong one to
   ask questions of, since `4294967295u` is that `Int`'s bits and not its value. A descriptor of its
   own is what keeps `1u as? Int` false and makes a boxed one render its value; the bits live in the
   signed field of the matching width, and only the descriptor says how to read them. */
KT_TYPE_WITH(kt_type_ubyte, "kotlin.UByte", sizeof(KObject), 0, NULL, kt_comparable_interfaces)
KT_TYPE_WITH(kt_type_ushort, "kotlin.UShort", sizeof(KObject), 0, NULL, kt_comparable_interfaces)
KT_TYPE_WITH(kt_type_uint, "kotlin.UInt", sizeof(KObject), 0, NULL, kt_comparable_interfaces)
KT_TYPE_WITH(kt_type_ulong, "kotlin.ULong", sizeof(KObject), 0, NULL, kt_comparable_interfaces)

#undef KT_TYPE

static KRef kt_new(const KType *type) { return (KRef)kt_gc_allocate(type, sizeof(KObject)); }

/* ---- strings ------------------------------------------------------------------------------- */

static KRef kt_string_of(KRef storage, const char *bytes, kt_int byte_length) {
    KRef object = kt_new(&kt_type_string);
    object->as.string.storage = storage;
    object->as.string.bytes = bytes;
    object->as.string.byte_length = byte_length;
    return object;
}

KRef kt_string_utf8(const char *bytes, kt_int byte_length) {
    return kt_string_of(NULL, bytes, byte_length);
}

/* The text a value holds, for the questions Kotlin asks about a string's CONTENT.
   Answering for a `StringBuilder` as well as a `String` is what lets `length`, `s[i]`, iteration
   and comparison serve both from one implementation, in the way `kt_list_size` already serves both
   list shapes: the question is about the text, and a builder has text. */
static const char *kt_text_of(KRef self, kt_int *byte_length);

/* Kotlin's `String.length` counts UTF-16 CODE UNITS; a krusty string holds UTF-8. The byte length
   is therefore not the answer, and neither is the code-point count. In UTF-8 a byte that is not a
   continuation byte (`10xxxxxx`) starts exactly one code point, so counting those counts code
   points; of those, only the ones a four-byte sequence starts (`11110xxx`, i.e. above U+FFFF) are
   written as a SURROGATE PAIR in UTF-16 and contribute two units. Everything else contributes one.

   This walks the bytes on every call, which is what a string that stores UTF-8 costs; it is also
   what makes the answer right for text a JVM-shaped length would have to be stored alongside. */
kt_int kt_string_length(KRef self) {
    /* Text the PROGRAM wrote: a class implementing `kotlin.CharSequence`, whose own `length` its
       descriptor records. Asked first, because `kt_text_of` reads a string's own storage and an
       object of the program's holds none. */
    if (self != NULL && self->header.type->walk_length != NULL) {
        return self->header.type->walk_length(self);
    }
    kt_int byte_length = 0;
    const char *bytes = kt_text_of(self, &byte_length);
    kt_int units = 0;
    for (kt_int index = 0; index < byte_length; index++) {
        unsigned char byte = (unsigned char)bytes[index];
        if ((byte & 0xC0u) == 0x80u) {
            continue;
        }
        units += (byte >= 0xF0u) ? 2 : 1;
    }
    return units;
}

/* `s[index]` — the UTF-16 code unit at `index`.

   The text is stored as UTF-8, and Kotlin indexes by UTF-16 unit, so this walks the bytes the same
   way `kt_string_length` counts them: a byte that is not a continuation byte starts one code point,
   and one above U+FFFF occupies TWO units. Walking per access is what a string that stores UTF-8
   costs, and it is the same cost `length` already pays; a program that wants to iterate cheaply
   iterates the string rather than its indices. */
kt_char kt_string_get(KRef self, kt_int index) {
    if (self != NULL && self->header.type->walk_char_at != NULL) {
        return self->header.type->walk_char_at(self, index);
    }
    kt_int byte_length = 0;
    const char *bytes = kt_text_of(self, &byte_length);
    kt_int unit = 0;
    for (kt_int at = 0; at < byte_length;) {
        unsigned char lead = (unsigned char)bytes[at];
        kt_int width = lead < 0x80u ? 1 : lead < 0xE0u ? 2 : lead < 0xF0u ? 3 : 4;
        kt_int units = width == 4 ? 2 : 1;
        if (index < unit + units) {
            uint32_t code = lead;
            if (width == 2) {
                code = ((uint32_t)(lead & 0x1Fu) << 6) | ((unsigned char)bytes[at + 1] & 0x3Fu);
            } else if (width == 3) {
                code = ((uint32_t)(lead & 0x0Fu) << 12) |
                       (((uint32_t)(unsigned char)bytes[at + 1] & 0x3Fu) << 6) |
                       ((unsigned char)bytes[at + 2] & 0x3Fu);
            } else if (width == 4) {
                code = ((uint32_t)(lead & 0x07u) << 18) |
                       (((uint32_t)(unsigned char)bytes[at + 1] & 0x3Fu) << 12) |
                       (((uint32_t)(unsigned char)bytes[at + 2] & 0x3Fu) << 6) |
                       ((unsigned char)bytes[at + 3] & 0x3Fu);
                /* Above the BMP: the pair Kotlin stores, high unit first. */
                uint32_t rest = code - 0x10000u;
                return (kt_char)(index == unit ? 0xD800u + (rest >> 10) : 0xDC00u + (rest & 0x3FFu));
            }
            return (kt_char)code;
        }
        unit += units;
        at += width;
    }
    kt_index_out_of_bounds(index, unit);
    return 0;
}

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

static KUnits kt_units_of(KRef self) {
    kt_int byte_length = 0;
    const char *bytes = kt_text_of(self, &byte_length);
    KUnits units = {bytes, byte_length, 0, 0};
    return units;
}

/* The next unit, or zero when the text is exhausted. */
static kt_boolean kt_units_next(KUnits *units, kt_char *out) {
    if (units->pending != 0) {
        *out = (kt_char)units->pending;
        units->pending = 0;
        return 1;
    }
    if (units->at >= units->byte_length) {
        return 0;
    }
    const char *bytes = units->bytes;
    kt_int at = units->at;
    unsigned char lead = (unsigned char)bytes[at];
    kt_int width = lead < 0x80u ? 1 : lead < 0xE0u ? 2 : lead < 0xF0u ? 3 : 4;
    uint32_t code = lead;
    if (width == 2) {
        code = ((uint32_t)(lead & 0x1Fu) << 6) | ((unsigned char)bytes[at + 1] & 0x3Fu);
    } else if (width == 3) {
        code = ((uint32_t)(lead & 0x0Fu) << 12)
               | (((uint32_t)(unsigned char)bytes[at + 1] & 0x3Fu) << 6)
               | ((unsigned char)bytes[at + 2] & 0x3Fu);
    } else if (width == 4) {
        uint32_t rest = (((uint32_t)(lead & 0x07u) << 18)
                         | (((uint32_t)(unsigned char)bytes[at + 1] & 0x3Fu) << 12)
                         | (((uint32_t)(unsigned char)bytes[at + 2] & 0x3Fu) << 6)
                         | ((unsigned char)bytes[at + 3] & 0x3Fu))
                        - 0x10000u;
        units->pending = 0xDC00u + (rest & 0x3FFu);
        code = 0xD800u + (rest >> 10);
    }
    units->at = at + width;
    *out = (kt_char)code;
    return 1;
}

kt_int kt_string_compare_to(KRef a, KRef b) {
    KUnits left = kt_units_of(a);
    KUnits right = kt_units_of(b);
    for (;;) {
        kt_char x = 0;
        kt_char y = 0;
        kt_boolean has_left = kt_units_next(&left, &x);
        kt_boolean has_right = kt_units_next(&right, &y);
        if (!has_left || !has_right) {
            /* One is a prefix of the other, or they are equal: the length difference, which is the
               magnitude Java's own `compareTo` answers and a program may print. */
            return kt_string_length(a) - kt_string_length(b);
        }
        if (x != y) {
            return (kt_int)x - (kt_int)y;
        }
    }
}

/* The BYTE offset at which UTF-16 unit `index` begins; `index` equal to the length answers the end
   of the text. */
static kt_int kt_string_offset(KRef self, kt_int index) {
    const char *bytes = self->as.string.bytes;
    kt_int byte_length = self->as.string.byte_length;
    kt_int unit = 0;
    kt_int at = 0;
    while (at < byte_length) {
        if (unit == index) {
            return at;
        }
        unsigned char lead = (unsigned char)bytes[at];
        kt_int width = lead < 0x80u ? 1 : lead < 0xE0u ? 2 : lead < 0xF0u ? 3 : 4;
        kt_int units = width == 4 ? 2 : 1;
        if (index < unit + units) {
            /* Between the halves of one character. Kotlin lets a program ask for this and answers
               with an unpaired surrogate; UTF-8 has no encoding for one, so there is no string to
               hand back and saying so is better than handing back a different text. */
            KT_FAIL("krusty: a string index inside a surrogate pair\n");
        }
        unit += units;
        at += width;
    }
    if (unit == index) {
        return at;
    }
    kt_index_out_of_bounds(index, unit);
    return 0;
}

KRef kt_string_substring(KRef self, kt_int start, kt_int end) {
    if (start < 0 || end < start) {
        kt_index_out_of_bounds(start, end);
    }
    kt_int from = kt_string_offset(self, start);
    kt_int to = kt_string_offset(self, end);
    /* The storage is shared, not copied: the receiver's own text already holds these bytes, and
       the collector keeps it alive through the field the new string names. */
    return kt_string_of(self->as.string.storage, self->as.string.bytes + from, to - from);
}

KRef kt_string_substring_from(KRef self, kt_int start) {
    kt_int from = kt_string_offset(self, start);
    kt_int length = self->as.string.byte_length;
    return kt_string_of(self->as.string.storage, self->as.string.bytes + from, length - from);
}

KRef kt_string_remove_suffix(KRef self, KRef suffix) {
    kt_int length = self->as.string.byte_length;
    kt_int tail = suffix->as.string.byte_length;
    if (tail > length) {
        return self;
    }
    const char *bytes = self->as.string.bytes;
    const char *wanted = suffix->as.string.bytes;
    for (kt_int index = 0; index < tail; index++) {
        if (bytes[length - tail + index] != wanted[index]) {
            return self;
        }
    }
    return kt_string_of(self->as.string.storage, bytes, length - tail);
}

/* The code point beginning at byte `at`, with the width of its encoding written to `width`.

   The UTF-16 walk above answers in UNITS, which is what Kotlin counts; the questions below —
   whitespace, reversal — are about CHARACTERS, and a character above U+FFFF is one of those and
   two of the other. Decoding once per character rather than per unit is what keeps a surrogate
   pair from being split by an operation that has no business splitting it. */
static uint32_t kt_code_point_at(const char *bytes, kt_int at, kt_int *width) {
    unsigned char lead = (unsigned char)bytes[at];
    if (lead < 0x80u) {
        *width = 1;
        return lead;
    }
    if (lead < 0xE0u) {
        *width = 2;
        return ((uint32_t)(lead & 0x1Fu) << 6) | ((unsigned char)bytes[at + 1] & 0x3Fu);
    }
    if (lead < 0xF0u) {
        *width = 3;
        return ((uint32_t)(lead & 0x0Fu) << 12)
               | (((uint32_t)(unsigned char)bytes[at + 1] & 0x3Fu) << 6)
               | ((unsigned char)bytes[at + 2] & 0x3Fu);
    }
    *width = 4;
    return ((uint32_t)(lead & 0x07u) << 18)
           | (((uint32_t)(unsigned char)bytes[at + 1] & 0x3Fu) << 12)
           | (((uint32_t)(unsigned char)bytes[at + 2] & 0x3Fu) << 6)
           | ((unsigned char)bytes[at + 3] & 0x3Fu);
}

/* Kotlin's `Char.isWhitespace()`, which is Java's `isWhitespace(c) || isSpaceChar(c)` — the UNION,
   so the non-breaking spaces that `isWhitespace` alone excludes (U+00A0, U+2007, U+202F) are
   whitespace here. Listing the code points is the whole of the definition: the set is closed and
   small, and a table lookup into a Unicode database would answer the same thing at more cost. */
static kt_boolean kt_is_whitespace(uint32_t code) {
    if (code <= 0x20u) {
        /* Tab, the line breaks, and the file/group/record/unit separators, plus the space. */
        return (code >= 0x09u && code <= 0x0Du) || (code >= 0x1Cu && code <= 0x20u);
    }
    if (code >= 0x2000u && code <= 0x200Au) {
        return 1;
    }
    return code == 0x85u || code == 0xA0u || code == 0x1680u || code == 0x2028u
           || code == 0x2029u || code == 0x202Fu || code == 0x205Fu || code == 0x3000u;
}

/* A string over the receiver's bytes from `from` (inclusive) to `to`, sharing the receiver's
   storage where it can.

   A STRING's text is immutable, so a slice of it is a view — the same trade `substring` makes. A
   BUILDER's is not: a later `append` may replace the very array the text lives in, so a string cut
   from one takes a copy. Both shapes reach here because the `kotlin.text` members these serve are
   declared on `CharSequence`. */
static KRef kt_string_slice(KRef self, kt_int from, kt_int to) {
    kt_int byte_length = 0;
    const char *bytes = kt_text_of(self, &byte_length);
    kt_int length = to - from;
    if (self->header.type == &kt_type_string_builder) {
        KByteArray *copied = kt_bytes_new(length);
        memcpy(kt_bytes_of(copied), bytes + from, (size_t)length);
        return kt_string_of((KRef)copied, kt_bytes_of(copied), length);
    }
    return kt_string_of(self->as.string.storage, self->as.string.bytes + from, length);
}

/* `s.isEmpty()` and `s.isNotEmpty()`. No walk is needed and none would help: a text has zero
   UTF-16 units exactly when it has zero bytes, since every encoding is at least one byte long. */
kt_boolean kt_string_is_empty(KRef self) {
    kt_int byte_length = 0;
    (void)kt_text_of(self, &byte_length);
    return byte_length == 0;
}

kt_boolean kt_string_is_not_empty(KRef self) { return !kt_string_is_empty(self); }

/* `s.isBlank()` and `s.isNotBlank()`: empty, or whitespace all the way through. */
kt_boolean kt_string_is_blank(KRef self) {
    kt_int byte_length = 0;
    const char *bytes = kt_text_of(self, &byte_length);
    for (kt_int at = 0; at < byte_length;) {
        kt_int width = 0;
        if (!kt_is_whitespace(kt_code_point_at(bytes, at, &width))) {
            return 0;
        }
        at += width;
    }
    return 1;
}

kt_boolean kt_string_is_not_blank(KRef self) { return !kt_string_is_blank(self); }

/* `s.trim()`, `s.trimStart()` and `s.trimEnd()`. The bounds are found by character and cut on a
   character boundary, so the result is always well-formed text. */
static void kt_string_trimmed(KRef self, kt_int *from, kt_int *to) {
    kt_int byte_length = 0;
    const char *bytes = kt_text_of(self, &byte_length);
    kt_int start = 0;
    while (start < byte_length) {
        kt_int width = 0;
        if (!kt_is_whitespace(kt_code_point_at(bytes, start, &width))) {
            break;
        }
        start += width;
    }
    kt_int end = byte_length;
    while (end > start) {
        /* Back up over the continuation bytes to the character's lead byte: the walk runs forward
           everywhere else, and this is the one place that needs the previous character. */
        kt_int back = end - 1;
        while (back > start && ((unsigned char)bytes[back] & 0xC0u) == 0x80u) {
            back--;
        }
        kt_int width = 0;
        if (!kt_is_whitespace(kt_code_point_at(bytes, back, &width))) {
            break;
        }
        end = back;
    }
    *from = start;
    *to = end;
}

KRef kt_string_trim(KRef self) {
    kt_int from = 0;
    kt_int to = 0;
    kt_string_trimmed(self, &from, &to);
    return kt_string_slice(self, from, to);
}

KRef kt_string_trim_start(KRef self) {
    kt_int from = 0;
    kt_int to = 0;
    kt_string_trimmed(self, &from, &to);
    kt_int byte_length = 0;
    (void)kt_text_of(self, &byte_length);
    return kt_string_slice(self, from, byte_length);
}

KRef kt_string_trim_end(KRef self) {
    kt_int from = 0;
    kt_int to = 0;
    kt_string_trimmed(self, &from, &to);
    return kt_string_slice(self, 0, to);
}

/* Whether the receiver's bytes hold `other`'s at `at`. */
static kt_boolean kt_bytes_match(const char *bytes, kt_int at, const char *wanted, kt_int length) {
    for (kt_int index = 0; index < length; index++) {
        if (bytes[at + index] != wanted[index]) {
            return 0;
        }
    }
    return 1;
}

/* `s.startsWith(prefix)`, `s.endsWith(suffix)` and `s.contains(other)`.

   Bytes settle all three, for the reason `removeSuffix` already relies on: UTF-8 is a prefix code,
   so one text begins, ends or holds another exactly when its bytes do — a match can neither start
   in the middle of a character nor straddle one. Only the case-SENSITIVE forms reach here; the
   generator declines `ignoreCase = true`, which is a question about Unicode case folding rather
   than about text. */
kt_boolean kt_string_starts_with(KRef self, KRef prefix) {
    kt_int byte_length = 0;
    const char *bytes = kt_text_of(self, &byte_length);
    kt_int head = 0;
    const char *wanted = kt_text_of(prefix, &head);
    return head <= byte_length && kt_bytes_match(bytes, 0, wanted, head);
}

kt_boolean kt_string_ends_with(KRef self, KRef suffix) {
    kt_int byte_length = 0;
    const char *bytes = kt_text_of(self, &byte_length);
    kt_int tail = 0;
    const char *wanted = kt_text_of(suffix, &tail);
    return tail <= byte_length && kt_bytes_match(bytes, byte_length - tail, wanted, tail);
}

kt_boolean kt_string_contains(KRef self, KRef other) {
    kt_int byte_length = 0;
    const char *bytes = kt_text_of(self, &byte_length);
    kt_int wanted_length = 0;
    const char *wanted = kt_text_of(other, &wanted_length);
    for (kt_int at = 0; at + wanted_length <= byte_length; at++) {
        if (kt_bytes_match(bytes, at, wanted, wanted_length)) {
            return 1;
        }
    }
    return 0;
}

/* `s.repeat(n)`. A negative count is Kotlin's own `IllegalArgumentException`. */
KRef kt_string_repeat(KRef self, kt_int count) {
    if (count < 0) {
        KRef message = kt_string_plus(kt_string_utf8("Count 'n' must be non-negative, but was ", 40),
                                      kt_to_string(kt_box_int(count)));
        kt_throw(kt_throwable_new(&kt_type_illegal_argument_exception,
                                  kt_string_plus(message, kt_string_utf8(".", 1))));
        /* `kt_throw` comes back; the length below would be negative. See `kt_string_first`. */
        return self;
    }
    kt_int byte_length = 0;
    const char *bytes = kt_text_of(self, &byte_length);
    KByteArray *joined = kt_bytes_new(byte_length * count);
    for (kt_int time = 0; time < count; time++) {
        memcpy(kt_bytes_of(joined) + time * byte_length, bytes, (size_t)byte_length);
    }
    return kt_string_of((KRef)joined, kt_bytes_of(joined), byte_length * count);
}

/* `s.reversed()`. Reversal is by CHARACTER, not by UTF-16 unit: Kotlin's own answer keeps a
   surrogate pair together, and so does moving whole UTF-8 encodings. */
KRef kt_string_reversed(KRef self) {
    kt_int byte_length = 0;
    const char *bytes = kt_text_of(self, &byte_length);
    KByteArray *reversed = kt_bytes_new(byte_length);
    char *out = kt_bytes_of(reversed);
    kt_int written = byte_length;
    for (kt_int at = 0; at < byte_length;) {
        kt_int width = 0;
        (void)kt_code_point_at(bytes, at, &width);
        written -= width;
        memcpy(out + written, bytes + at, (size_t)width);
        at += width;
    }
    return kt_string_of((KRef)reversed, out, byte_length);
}

/* `s.first()` and `s.last()` — the UTF-16 unit at either end, which is what a `Char` is. Kotlin
   raises `NoSuchElementException` on empty text, with its own wording.

   The raise is followed by a RETURN rather than by the answer. `kt_throw` records the exception for
   the call site to find and COMES BACK, so whatever follows it runs with one already in flight —
   and `kt_string_get` on empty text raises its own, which would take this one's place and report an
   index the program never asked about. The value returned here is never read: the call site tests
   for the exception before it looks at the answer. */
static kt_boolean kt_text_raise_when_empty(KRef self) {
    if (!kt_string_is_empty(self)) {
        return 0;
    }
    kt_throw(kt_throwable_new(&kt_type_no_such_element_exception,
                              kt_string_utf8("Char sequence is empty.", 23)));
    return 1;
}

kt_char kt_string_first(KRef self) {
    if (kt_text_raise_when_empty(self)) {
        return 0;
    }
    return kt_string_get(self, 0);
}

kt_char kt_string_last(KRef self) {
    if (kt_text_raise_when_empty(self)) {
        return 0;
    }
    return kt_string_get(self, kt_string_length(self) - 1);
}

static kt_int kt_render_ulong(uint64_t value, char *buffer);

/* Render a signed 64-bit value into `buffer` (at least 20 bytes); returns the length written. */
static kt_int kt_render_long(kt_long value, char *buffer) {
    char digits[20];
    kt_int count = 0;
    /* Negate into UNSIGNED space: `-Long.MIN_VALUE` does not exist as a signed value. */
    uint64_t magnitude = value < 0 ? (0u - (uint64_t)value) : (uint64_t)value;
    do {
        digits[count++] = (char)('0' + (magnitude % 10u));
        magnitude /= 10u;
    } while (magnitude != 0);

    kt_int length = 0;
    if (value < 0) {
        buffer[length++] = '-';
    }
    while (count > 0) {
        buffer[length++] = digits[--count];
    }
    return length;
}

/* A `Char` is one UTF-16 code unit; the BMP subset encodes directly as UTF-8. */
static kt_int kt_render_char(kt_char unit, char *buffer) {
    if (unit < 0x80) {
        buffer[0] = (char)unit;
        return 1;
    }
    if (unit < 0x800) {
        buffer[0] = (char)(0xC0 | (unit >> 6));
        buffer[1] = (char)(0x80 | (unit & 0x3F));
        return 2;
    }
    buffer[0] = (char)(0xE0 | (unit >> 12));
    buffer[1] = (char)(0x80 | ((unit >> 6) & 0x3F));
    buffer[2] = (char)(0x80 | (unit & 0x3F));
    return 3;
}

static KRef kt_object_to_string(KRef value);

/* Render any value as bytes. `*storage` receives the heap object that owns the bytes (NULL when
   they are in static storage); a caller that allocates before it has finished with the bytes
   must keep it in a local, so the collector sees a root. */
static const char *kt_render(KRef value, kt_int *byte_length, KRef *storage) {
    *storage = NULL;
    if (value == NULL) {
        *byte_length = 4;
        return "null";
    }
    const KType *type = value->header.type;
    if (type == &kt_type_string) {
        *storage = value->as.string.storage;
        *byte_length = value->as.string.byte_length;
        return value->as.string.bytes;
    }
    if (type == &kt_type_boolean) {
        if (value->as.boolean_value) {
            *byte_length = 4;
            return "true";
        }
        *byte_length = 5;
        return "false";
    }
    if (type == &kt_type_unit) {
        *byte_length = 11;
        return "kotlin.Unit";
    }
    if (type == &kt_type_char) {
        KByteArray *buffer = kt_bytes_new(4);
        *byte_length = kt_render_char(value->as.char_value, kt_bytes_of(buffer));
        *storage = (KRef)buffer;
        return kt_bytes_of(buffer);
    }
    if (type == &kt_type_double || type == &kt_type_float) {
        KByteArray *buffer = kt_bytes_new(32);
        *byte_length = type == &kt_type_double
                           ? kt_render_double(value->as.double_value, kt_bytes_of(buffer))
                           : kt_render_float(value->as.float_value, kt_bytes_of(buffer));
        *storage = (KRef)buffer;
        return kt_bytes_of(buffer);
    }
    /* The four unsigned types share their storage with the signed one of the same width, so only
       the descriptor says how to read the bits — and reading them the other way is exactly the
       `4294967295u` printing `-1` this separation exists to prevent. */
    if (type == &kt_type_ubyte || type == &kt_type_ushort || type == &kt_type_uint
        || type == &kt_type_ulong) {
        uint64_t unsigned_value = type == &kt_type_ubyte    ? (uint8_t)value->as.byte_value
                                  : type == &kt_type_ushort ? (uint16_t)value->as.short_value
                                  : type == &kt_type_uint   ? (uint32_t)value->as.int_value
                                                            : (uint64_t)value->as.long_value;
        KByteArray *buffer = kt_bytes_new(24);
        *byte_length = kt_render_ulong(unsigned_value, kt_bytes_of(buffer));
        *storage = (KRef)buffer;
        return kt_bytes_of(buffer);
    }
    kt_long number;
    if (type == &kt_type_byte) {
        number = value->as.byte_value;
    } else if (type == &kt_type_short) {
        number = value->as.short_value;
    } else if (type == &kt_type_int) {
        number = value->as.int_value;
    } else if (type == &kt_type_long) {
        number = value->as.long_value;
    } else {
        /* A class instance: its own toString, through the vtable, so `println(obj)` and `"$obj"`
           reach a user override. The result is a string; its text is what gets rendered, and the
           text's storage is what the caller must keep alive. */
        KRef text = kt_object_to_string(value);
        if (text == NULL || text->header.type != &kt_type_string) {
            *byte_length = 4;
            return "null";
        }
        *storage = text->as.string.storage;
        *byte_length = text->as.string.byte_length;
        return text->as.string.bytes;
    }
    KByteArray *buffer = kt_bytes_new(24);
    *byte_length = kt_render_long(number, kt_bytes_of(buffer));
    *storage = (KRef)buffer;
    return kt_bytes_of(buffer);
}

KRef kt_to_string(KRef value) {
    if (value != NULL && value->header.type == &kt_type_string) {
        return value;
    }
    if (value != NULL && value->header.type->super != NULL && value->header.type->vtable != kt_builtin_vtable) {
        return kt_object_to_string(value);
    }
    kt_int length = 0;
    KRef storage = NULL;
    const char *bytes = kt_render(value, &length, &storage);
    return kt_string_of(storage, bytes, length);
}

KRef kt_string_plus(KRef a, KRef b) {
    kt_int left_length = 0;
    kt_int right_length = 0;
    /* Both storages stay in locals across the allocation below: they are its roots. */
    KRef left_storage = NULL;
    KRef right_storage = NULL;
    const char *left = kt_render(a, &left_length, &left_storage);
    const char *right = kt_render(b, &right_length, &right_storage);
    KByteArray *joined = kt_bytes_new(left_length + right_length);
    memcpy(kt_bytes_of(joined), left, (size_t)left_length);
    memcpy(kt_bytes_of(joined) + left_length, right, (size_t)right_length);
    return kt_string_of((KRef)joined, kt_bytes_of(joined), left_length + right_length);
}

/* ---- string builders ------------------------------------------------------------------------

   `kotlin.text.StringBuilder` is a growable UTF-8 buffer, laid out like the growable list above and
   for the same reasons: one reference field the collector traces, a written length beside it, and
   doubling so that repeated `append` stays linear. The capacity is the storage array's length; the
   text is its first `byte_length` bytes.

   `toString` COPIES rather than sharing the storage the way `substring` does. A string is a value
   and a builder is not: hand out a view and the next `append` rewrites text a program already
   holds. */

typedef struct KStringBuilder {
    KObjectHeader header;
    KRef storage;
    kt_int byte_length;
} KStringBuilder;

static const uint32_t kt_string_builder_offsets[] = {offsetof(KStringBuilder, storage)};

static KRef kt_string_builder_to_string(KRef self);

/* `equals` and `hashCode` are IDENTITY, which is what Kotlin answers here: `StringBuilder` does not
   override either, so two builders holding the same text are different objects and stay that way.
   Only `toString` is its own. */
static const kt_fn kt_string_builder_vtable[] = {
    (kt_fn)kt_any_equals, (kt_fn)kt_any_hash_code, (kt_fn)kt_string_builder_to_string};

const KType kt_type_string_builder = {"kotlin.text.StringBuilder",
                                      sizeof("kotlin.text.StringBuilder") - 1,
                                      sizeof(KStringBuilder),
                                      1,
                                      0,
                                      kt_string_builder_offsets,
                                      &kt_type_any,
                                      kt_string_builder_vtable,
                                      3,
                                      0,
                                      kt_char_sequence_interfaces,
                                      1};

static const char *kt_text_of(KRef self, kt_int *byte_length) {
    if (self->header.type == &kt_type_string_builder) {
        const KStringBuilder *builder = (const KStringBuilder *)self;
        *byte_length = builder->byte_length;
        /* An empty builder has a zero-length array, whose body is still a valid address to name. */
        return kt_bytes_of((KByteArray *)builder->storage);
    }
    *byte_length = self->as.string.byte_length;
    return self->as.string.bytes;
}

KRef kt_string_builder_with_capacity(kt_int capacity) {
    KStringBuilder *builder =
        (KStringBuilder *)kt_gc_allocate(&kt_type_string_builder, sizeof(KStringBuilder));
    builder->byte_length = 0;
    /* Stored before the array is allocated, so a collection triggered by that allocation never
       traces an uninitialized field. */
    builder->storage = NULL;
    builder->storage = (KRef)kt_bytes_new(capacity > 0 ? capacity : 0);
    return (KRef)builder;
}

KRef kt_string_builder_new(void) { return kt_string_builder_with_capacity(0); }

/* `StringBuilder(text)`: a builder that starts out holding it. A COPY, for the reason `toString`
   copies -- the text is a value and the builder is about to be written through. */
KRef kt_string_builder_with_text(KRef text) {
    kt_int length = 0;
    (void)kt_text_of(text, &length);
    /* `text` stays live in this parameter across the allocation. */
    KRef builder = kt_string_builder_with_capacity(length);
    const char *bytes = kt_text_of(text, &length);
    memcpy(kt_bytes_of((KByteArray *)((KStringBuilder *)builder)->storage), bytes, (size_t)length);
    ((KStringBuilder *)builder)->byte_length = length;
    return builder;
}

/* Grow to hold `additional` more bytes, doubling so repeated `append` stays linear overall. */
static void kt_string_builder_reserve(KRef self, kt_int additional) {
    KStringBuilder *builder = (KStringBuilder *)self;
    kt_int capacity = kt_length_of(builder->storage);
    kt_int needed = builder->byte_length + additional;
    if (needed <= capacity) {
        return;
    }
    kt_int grown = capacity == 0 ? 16 : capacity;
    while (grown < needed) {
        grown *= 2;
    }
    /* `self` is a root in the caller's frame, so the OLD array stays reachable through it until the
       new one is stored. */
    KRef replacement = kt_array_new(&kt_type_byte_array, grown);
    memcpy(kt_bytes_of((KByteArray *)replacement), kt_bytes_of((KByteArray *)builder->storage),
           (size_t)builder->byte_length);
    builder->storage = replacement;
}

/* `sb.setLength(n)`, by UTF-16 UNIT as Kotlin counts. Shorter truncates; longer pads with NUL,
   which is what Java's own does and what a program reading the result back would see.

   Truncating cuts on a character boundary, so `kt_string_offset` finds the byte — and asking for a
   length inside a surrogate pair is the loud failure it already is, for the same reason: UTF-8 has
   no encoding for half a character. A NUL is one byte, so padding costs one per unit. */
void kt_string_builder_set_length(KRef self, kt_int length) {
    if (length < 0) {
        kt_index_out_of_bounds(length, kt_string_length(self));
        return;
    }
    kt_int units = kt_string_length(self);
    if (length <= units) {
        KStringBuilder *builder = (KStringBuilder *)self;
        /* The text is the builder's own bytes, which `kt_string_of` names without copying: the
           offset wanted is a byte count into them. */
        KRef view = kt_string_of(builder->storage, kt_bytes_of((KByteArray *)builder->storage),
                                 builder->byte_length);
        builder->byte_length = kt_string_offset(view, length);
        return;
    }
    kt_int padding = length - units;
    kt_string_builder_reserve(self, padding);
    KStringBuilder *builder = (KStringBuilder *)self;
    memset(kt_bytes_of((KByteArray *)builder->storage) + builder->byte_length, 0,
           (size_t)padding);
    builder->byte_length += padding;
}

KRef kt_string_builder_append(KRef self, KRef value) {
    /* The rendering goes through `kt_to_string` rather than `kt_render`, because a value whose type
       overrides `toString` must answer with ITS text and only the vtable knows that. It allocates,
       and the result is held in a local across the reserve below so the collector sees the root. */
    KRef text = kt_to_string(value);
    kt_int length = 0;
    const char *bytes = kt_text_of(text, &length);
    kt_string_builder_reserve(self, length);
    KStringBuilder *builder = (KStringBuilder *)self;
    /* `bytes` is re-read after the reserve: it may point into storage the reserve replaced, when a
       builder is appended to itself. */
    bytes = kt_text_of(text, &length);
    memcpy(kt_bytes_of((KByteArray *)builder->storage) + builder->byte_length, bytes,
           (size_t)length);
    builder->byte_length += length;
    return self;
}

/* `appendLine(value)` — the value then a newline, which is what Kotlin's own appends on every
   target: `StringBuilder.appendLine` is specified as `\n` and not as the platform separator. */
KRef kt_string_builder_append_line(KRef self, KRef value) {
    self = kt_string_builder_append(self, value);
    kt_string_builder_reserve(self, 1);
    KStringBuilder *builder = (KStringBuilder *)self;
    kt_bytes_of((KByteArray *)builder->storage)[builder->byte_length] = '\n';
    builder->byte_length += 1;
    return self;
}

/* `appendLine()` with nothing to append: the newline alone, NOT the text `"null"` that
   `appendLine(null)` would add. */
KRef kt_string_builder_append_new_line(KRef self) {
    kt_string_builder_reserve(self, 1);
    KStringBuilder *builder = (KStringBuilder *)self;
    kt_bytes_of((KByteArray *)builder->storage)[builder->byte_length] = '\n';
    builder->byte_length += 1;
    return self;
}

static KRef kt_string_builder_to_string(KRef self) {
    const KStringBuilder *builder = (const KStringBuilder *)self;
    kt_int length = builder->byte_length;
    /* `self` stays a root in the caller's frame across this allocation. */
    KByteArray *copied = kt_bytes_new(length);
    memcpy(kt_bytes_of(copied), kt_bytes_of((KByteArray *)builder->storage), (size_t)length);
    return kt_string_of((KRef)copied, kt_bytes_of(copied), length);
}

kt_boolean kt_is_string_builder(KRef value) {
    return value != NULL && value->header.type == &kt_type_string_builder;
}

/* ---- boxing -------------------------------------------------------------------------------- */

/* Boxing a SMALL value hands out the same object every time, and a program can see that:
   `boxBoolean(true) === boxBoolean(true)` is true in Kotlin. The cached range is the one the JVM
   specifies and Kotlin/Native also caches — every `Byte`, `Short`/`Int`/`Long` in -128..127,
   `Char` in 0..127, and both `Boolean`s. Outside it a box is a fresh object and identity is
   unspecified, which is what Kotlin says as well; nothing here promises more than that.

   The cache is static storage, not the heap, and that is deliberate on two counts: these objects
   must outlive every collection, and the collector ignores them for free — both the conservative
   root scan and the precise field tracer resolve a candidate address to its heap chunk and drop
   one that belongs to no chunk. An entry is filled on first use rather than at startup, with a
   NULL type as the "not yet" marker (static storage starts zeroed), so the runtime pays for only
   the values a program actually boxes. */
#define KT_BOX(suffix, type_descriptor, field, carrier, low, high)                                 \
    static KObject kt_cache_##suffix[(high) - (low) + 1];                                          \
    KRef kt_box_##suffix(carrier value) {                                                          \
        /* The WHOLE value picks the slot, never its low word: `Long.MIN_VALUE` truncated to an    \
           `int` is zero, which put it in zero's slot and handed it back as zero from then on. */  \
        kt_long slot = (kt_long)value;                                                             \
        if (slot >= (low) && slot <= (high)) {                                                     \
            KObject *cached = &kt_cache_##suffix[(int)(slot - (low))];                             \
            if (cached->header.type == NULL) {                                                     \
                cached->header.type = &type_descriptor;                                            \
                cached->as.field = value;                                                          \
            }                                                                                      \
            return cached;                                                                         \
        }                                                                                          \
        KRef object = kt_new(&type_descriptor);                                                    \
        object->as.field = value;                                                                  \
        return object;                                                                             \
    }

KT_BOX(byte, kt_type_byte, byte_value, kt_byte, -128, 127)
KT_BOX(short, kt_type_short, short_value, kt_short, -128, 127)
KT_BOX(int, kt_type_int, int_value, kt_int, -128, 127)
KT_BOX(long, kt_type_long, long_value, kt_long, -128, 127)
KT_BOX(char, kt_type_char, char_value, kt_char, 0, 127)
KT_BOX(boolean, kt_type_boolean, boolean_value, kt_boolean, 0, 1)

#undef KT_BOX

/* No small-value cache for these two: `(int)value` would round, so `0.5` and `0.0` would share a
   cache slot and a boxed `0.5` would come back `0.0`. */
KRef kt_box_float(kt_float value) {
    KRef object = kt_new(&kt_type_float);
    object->as.float_value = value;
    return object;
}

KRef kt_box_double(kt_double value) {
    KRef object = kt_new(&kt_type_double);
    object->as.double_value = value;
    return object;
}

/* Unboxing a `null` is Kotlin's `NullPointerException` — the one `!!` raises, so with no message.
   The zero returned afterwards is never read: the caller checks the pending slot first. */
#define KT_UNBOX(suffix, field, type)                                                              \
    type kt_unbox_##suffix(KRef value) {                                                           \
        if (value == NULL) {                                                                       \
            kt_throw(kt_throwable_new(&kt_type_null_pointer_exception, NULL));                     \
        }                                                                                          \
        return value->as.field;                                                                    \
    }

KT_UNBOX(byte, byte_value, kt_byte)
KT_UNBOX(short, short_value, kt_short)
KT_UNBOX(int, int_value, kt_int)
KT_UNBOX(long, long_value, kt_long)
KT_UNBOX(char, char_value, kt_char)
KT_UNBOX(boolean, boolean_value, kt_boolean)
KT_UNBOX(float, float_value, kt_float)
KT_UNBOX(double, double_value, kt_double)

#undef KT_UNBOX

/* What is in a `Number` box, read through its descriptor. A floating-point source is kept as one
   rather than folded into the integer, because `Double.toLong()` saturates where a cast would not
   and `(long)NaN` is not a value C defines at all. */
typedef struct KNumber {
    kt_boolean is_real;
    kt_long integer;
    kt_double real;
} KNumber;

static KNumber kt_number_of(KRef value) {
    if (value == NULL) {
        kt_throw(kt_throwable_new(&kt_type_null_pointer_exception, NULL));
    }
    const KType *type = value->header.type;
    KNumber number = {0, 0, 0.0};
    if (type == &kt_type_byte) {
        number.integer = value->as.byte_value;
    } else if (type == &kt_type_short) {
        number.integer = value->as.short_value;
    } else if (type == &kt_type_int) {
        number.integer = value->as.int_value;
    } else if (type == &kt_type_long) {
        number.integer = value->as.long_value;
    } else if (type == &kt_type_float) {
        number.is_real = 1;
        number.real = value->as.float_value;
    } else if (type == &kt_type_double) {
        number.is_real = 1;
        number.real = value->as.double_value;
    } else {
        KT_FAIL("krusty: a kotlin.Number member on a value that is not a number\n");
    }
    return number;
}

/* Kotlin's floating-point to integer conversion: `NaN` is zero and everything outside the target's
   range clamps to its nearest end. C would make both of those undefined, so neither is a cast. */
static kt_long kt_saturate_long(kt_double value) {
    if (value != value) {
        return 0;
    }
    if (value >= 9223372036854775808.0) {
        return (kt_long)0x7fffffffffffffffLL;
    }
    if (value <= -9223372036854775808.0) {
        return (kt_long)(-0x7fffffffffffffffLL - 1);
    }
    return (kt_long)value;
}

static kt_int kt_saturate_int(kt_double value) {
    if (value != value) {
        return 0;
    }
    if (value >= 2147483648.0) {
        return (kt_int)0x7fffffff;
    }
    if (value <= -2147483648.0) {
        return (kt_int)(-0x7fffffff - 1);
    }
    return (kt_int)value;
}

kt_long kt_number_to_long(KRef value) {
    KNumber number = kt_number_of(value);
    return number.is_real ? kt_saturate_long(number.real) : number.integer;
}

kt_int kt_number_to_int(KRef value) {
    KNumber number = kt_number_of(value);
    return number.is_real ? kt_saturate_int(number.real) : (kt_int)number.integer;
}

/* `Double.toShort()` is `toInt().toShort()` in Kotlin, and a wider integer simply truncates —
   the same answer either way, which is why both go through `toInt` here. */
kt_short kt_number_to_short(KRef value) { return (kt_short)kt_number_to_int(value); }

kt_byte kt_number_to_byte(KRef value) { return (kt_byte)kt_number_to_int(value); }

kt_float kt_number_to_float(KRef value) {
    KNumber number = kt_number_of(value);
    return number.is_real ? (kt_float)number.real : (kt_float)number.integer;
}

kt_double kt_number_to_double(KRef value) {
    KNumber number = kt_number_of(value);
    return number.is_real ? number.real : (kt_double)number.integer;
}

/* ---- class literals -------------------------------------------------------------------------- */

/* The descriptor is STATIC storage, not a heap object: the collector resolves a candidate address
   to its chunk and drops one that belongs to none, so this field is neither traced nor needs to be
   — which is why the type below declares no references. */
typedef struct KClass {
    KObjectHeader header;
    const KType *described;
} KClass;

static kt_boolean kt_class_equals(KRef self, KRef other);
static kt_int kt_class_hash_code(KRef self);
static KRef kt_class_to_string(KRef self);

static const kt_fn kt_class_vtable[] = {(kt_fn)kt_class_equals, (kt_fn)kt_class_hash_code,
                                        (kt_fn)kt_class_to_string};

const KType kt_type_kclass = {"kotlin.reflect.KClass",
                              sizeof("kotlin.reflect.KClass") - 1,
                              sizeof(KClass),
                              0,
                              0,
                              NULL,
                              &kt_type_any,
                              kt_class_vtable,
                              3,
                              0};

KRef kt_class_literal(const KType *type) {
    KClass *literal = (KClass *)kt_gc_allocate(&kt_type_kclass, sizeof(KClass));
    literal->described = type;
    return (KRef)literal;
}

KRef kt_class_of(KRef value) {
    if (value == NULL) {
        KT_FAIL("krusty: member access on a null receiver\n");
    }
    return kt_class_literal(value->header.type);
}

/* Equality is the TYPE, not the object. Kotlin's `KClass` is equal by the class it stands for —
   `x::class == String::class` is the question programs actually ask — and answering it this way is
   what lets a literal be an ordinary allocation instead of a canonical instance the runtime would
   have to keep a table of. */
static kt_boolean kt_class_equals(KRef self, KRef other) {
    if (other == NULL || other->header.type != &kt_type_kclass) {
        return 0;
    }
    return ((const KClass *)self)->described == ((const KClass *)other)->described;
}

static kt_int kt_class_hash_code(KRef self) {
    /* The descriptor's address, which is stable: it is static storage and the collector never
       moves anything. Folded to 32 bits the way the object hash already is. */
    uintptr_t address = (uintptr_t)((const KClass *)self)->described;
    return (kt_int)(uint32_t)((address >> 4) ^ (address >> 36));
}

/* `class kotlin.String`, which is what Kotlin's own `KClass.toString` prints. */
static KRef kt_class_to_string(KRef self) {
    const KType *described = ((const KClass *)self)->described;
    KRef prefix = kt_string_utf8("class ", 6);
    KRef name = kt_string_utf8(described->name, (kt_int)described->name_length);
    return kt_string_plus(prefix, name);
}

KRef kt_class_qualified_name(KRef self) {
    const KType *described = ((const KClass *)self)->described;
    return kt_string_utf8(described->name, (kt_int)described->name_length);
}

/* The last segment of the qualified name. A name with no separator is its own simple name, which
   is what a class in the root package has.

   Both separators count. A package is spelled with dots and NESTING with `$` — `A$Companion` is
   the companion of `A` — so splitting on dots alone answered the whole nested name where Kotlin
   answers `Companion`. */
KRef kt_class_simple_name(KRef self) {
    const KType *described = ((const KClass *)self)->described;
    kt_int start = 0;
    for (kt_int at = 0; at < (kt_int)described->name_length; at++) {
        if (described->name[at] == '.' || described->name[at] == '$') {
            start = at + 1;
        }
    }
    return kt_string_utf8(described->name + start, (kt_int)described->name_length - start);
}

/* ---- lazy ---------------------------------------------------------------------------------- */

typedef struct KLazy {
    KObjectHeader header;
    /* The initializer while it is still needed, NULL once the value has been computed. */
    KRef initializer;
    KRef value;
    kt_boolean computed;
} KLazy;

static const uint32_t kt_lazy_offsets[] = {offsetof(KLazy, initializer), offsetof(KLazy, value)};

static KRef kt_lazy_to_string(KRef self);

/* `equals` and `hashCode` are identity, as Kotlin's `Lazy` leaves them — it is not a data class,
   and two separately created lazies are two objects whatever they hold. */
static const kt_fn kt_lazy_vtable[] = {(kt_fn)kt_any_equals, (kt_fn)kt_any_hash_code,
                                       (kt_fn)kt_lazy_to_string};

const KType kt_type_lazy = {"kotlin.Lazy",
                            sizeof("kotlin.Lazy") - 1,
                            sizeof(KLazy),
                            2,
                            0,
                            kt_lazy_offsets,
                            &kt_type_any,
                            kt_lazy_vtable,
                            3,
                            0};

/* `kotlin.Result` is a value class over `Any?`, and its representation is Kotlin's own: a SUCCESS
   is the value itself, so `Result.success(x)` is `x` and costs nothing, and a FAILURE is this
   marker holding the exception. That is what lets a `Result<T>` cross a function boundary as an
   ordinary reference with no wrapper of this runtime's invention.

   A `null` success is representable and distinct from a failure, because a failure is never NULL. */
typedef struct KResultFailure {
    KObjectHeader header;
    KRef exception;
} KResultFailure;

static const uint32_t kt_result_failure_offsets[] = {offsetof(KResultFailure, exception)};

static const KType kt_type_result_failure = {"kotlin.Result.Failure",
                                             sizeof("kotlin.Result.Failure") - 1,
                                             sizeof(KResultFailure),
                                             1,
                                             0,
                                             kt_result_failure_offsets,
                                             &kt_type_any,
                                             kt_any_vtable,
                                             3,
                                             0};

/* `Result.success(x)` IS `x`. This exists so the call site has a target of the ordinary shape
   rather than a special case; it costs one call and no allocation. */
KRef kt_result_success(KRef value) { return value; }

KRef kt_result_failure(KRef exception) {
    KResultFailure *failure =
        (KResultFailure *)kt_gc_allocate(&kt_type_result_failure, sizeof(KResultFailure));
    failure->exception = exception;
    return (KRef)failure;
}

kt_boolean kt_result_is_failure(KRef value) {
    return value != NULL && value->header.type == &kt_type_result_failure;
}

kt_boolean kt_result_is_success(KRef value) { return !kt_result_is_failure(value); }

KRef kt_result_get_or_null(KRef value) { return kt_result_is_failure(value) ? NULL : value; }

KRef kt_result_exception_or_null(KRef value) {
    return kt_result_is_failure(value) ? ((const KResultFailure *)value)->exception : NULL;
}

KRef kt_result_get_or_throw(KRef value) {
    if (kt_result_is_failure(value)) {
        kt_throw(((const KResultFailure *)value)->exception);
    }
    return value;
}

/* Kotlin's own rendering: `Success(value)` or `Failure(exception)`. */
KRef kt_result_to_string(KRef value) {
    if (kt_result_is_failure(value)) {
        KRef opening = kt_string_utf8("Failure(", 8);
        KRef rendered = kt_to_string(((const KResultFailure *)value)->exception);
        return kt_string_plus(kt_string_plus(opening, rendered), kt_string_utf8(")", 1));
    }
    KRef opening = kt_string_utf8("Success(", 8);
    return kt_string_plus(kt_string_plus(opening, kt_to_string(value)), kt_string_utf8(")", 1));
}

KRef kt_lazy_of(KRef initializer) {
    KLazy *lazy = (KLazy *)kt_gc_allocate(&kt_type_lazy, sizeof(KLazy));
    lazy->initializer = initializer;
    lazy->value = NULL;
    lazy->computed = false;
    return (KRef)lazy;
}

kt_boolean kt_lazy_is_initialized(KRef lazy) { return ((const KLazy *)lazy)->computed; }

/* The value, computing it on the first ask. This is the one place the runtime CALLS back into
   emitted code: the initializer is a `Function0`, and a function value answers `invoke` in the one
   vtable slot it declares beyond `kotlin.Any`'s three. */
KRef kt_lazy_value(KRef lazy) {
    KLazy *self = (KLazy *)lazy;
    if (self->computed) {
        return self->value;
    }
    KRef initializer = self->initializer;
    if (initializer == NULL || initializer->header.type->vtable == NULL ||
        initializer->header.type->vtable_length <= KT_SLOT_INVOKE) {
        KT_FAIL("krusty: a lazy value has no initializer to run\n");
    }
    KRef value =
        ((KRef(*)(KRef))initializer->header.type->vtable[KT_SLOT_INVOKE])(initializer);
    /* Re-read through `lazy`: the call above can collect, and `self` is a root only because it is
       this local. The collector never moves an object, so the pointer is still good. */
    self->value = value;
    self->computed = true;
    self->initializer = NULL;
    return value;
}

/* Kotlin's own: the value once there is one, and a fixed text before that — which is the whole
   point of `Lazy.toString`, that asking for it must not force the value. */
static KRef kt_lazy_to_string(KRef self) {
    const KLazy *lazy = (const KLazy *)self;
    if (!lazy->computed) {
        return kt_string_utf8("Lazy value not initialized yet.", 31);
    }
    return kt_to_string(lazy->value);
}

/* ---- Delegates.notNull ------------------------------------------------------------------------

   `var x: T by Delegates.notNull()`. One reference and the rule that reading it before it is
   written is an error — Kotlin's `NotNullVar`, whose whole content is that check. `null` is not a
   value it can hold (its `T` is non-null by declaration), so the empty slot needs no flag beside
   it the way a lazy's `computed` does.

   The message names the PROPERTY, as Kotlin's does. The name is a string the generator hands over,
   read at the call site from the `KProperty` operand the delegate convention passes: the runtime
   cannot ask the object for it, since a property reference answers `name` from a table of the
   emitted code's own. */
static void kt_raise_uninitialized_property(KRef name);
static KRef kt_invoke_three(KRef function, KRef first, KRef second, KRef third);

typedef struct KNotNullVar {
    KObjectHeader header;
    KRef value;
} KNotNullVar;

static const uint32_t kt_not_null_var_offsets[] = {offsetof(KNotNullVar, value)};

/* Identity, as Kotlin leaves them: `NotNullVar` is no data class, and `toString` is `Any`'s. */
static const kt_fn kt_not_null_var_vtable[] = {(kt_fn)kt_any_equals, (kt_fn)kt_any_hash_code,
                                           (kt_fn)kt_any_to_string};

const KType kt_type_not_null_var = {"kotlin.properties.NotNullVar",
                                sizeof("kotlin.properties.NotNullVar") - 1,
                                sizeof(KNotNullVar),
                                1,
                                0,
                                kt_not_null_var_offsets,
                                &kt_type_any,
                                kt_not_null_var_vtable,
                                3,
                                0};

KRef kt_not_null_var(void) {
    KNotNullVar *var = (KNotNullVar *)kt_gc_allocate(&kt_type_not_null_var, sizeof(KNotNullVar));
    var->value = NULL;
    return (KRef)var;
}

/* `Delegates.observable(initial) { property, old, new -> … }`. Kotlin's `ObservableProperty`: the
   value, and a callback run AFTER each write with the property and both values. The callback is an
   ordinary function value, invoked through the one slot every function value declares.

   The `KProperty` is not read here, only passed along — which is what lets this runtime carry it
   without any reflection: whatever object the emitted code built for the delegation, the callback
   receives that same object. */
typedef struct KObservable {
    KObjectHeader header;
    KRef value;
    KRef on_change;
} KObservable;

static const uint32_t kt_observable_offsets[] = {offsetof(KObservable, value),
                                                 offsetof(KObservable, on_change)};

static const kt_fn kt_observable_vtable[] = {(kt_fn)kt_any_equals, (kt_fn)kt_any_hash_code,
                                             (kt_fn)kt_any_to_string};

const KType kt_type_observable = {"kotlin.properties.ObservableProperty",
                                  sizeof("kotlin.properties.ObservableProperty") - 1,
                                  sizeof(KObservable),
                                  2,
                                  0,
                                  kt_observable_offsets,
                                  &kt_type_any,
                                  kt_observable_vtable,
                                  3,
                                  0};

KRef kt_observable(KRef initial, KRef on_change) {
    KObservable *observable =
        (KObservable *)kt_gc_allocate(&kt_type_observable, sizeof(KObservable));
    observable->value = initial;
    observable->on_change = on_change;
    return (KRef)observable;
}

/* The two entry points a `ReadWriteProperty` receiver reaches, dispatching on the DESCRIPTOR. No
   static type separates the delegates this runtime builds — `notNull()` and `observable(…)` are
   both a `ReadWriteProperty<Any?, T>` at the call site — so the object says which it is, exactly as
   every other runtime answer here does.

   `get` is handed the property's NAME rather than the property, because the only thing it can need
   is the text of the error a `notNull` read-before-write raises. `set` is handed the PROPERTY,
   because an observable passes it to the callback. */
KRef kt_rw_property_get(KRef self, KRef name) {
    if (self == NULL) {
        KT_FAIL("krusty: member access on a null receiver\n");
    }
    if (self->header.type == &kt_type_observable) {
        return ((const KObservable *)self)->value;
    }
    KRef value = ((const KNotNullVar *)self)->value;
    if (value == NULL) {
        kt_raise_uninitialized_property(name);
        return NULL;
    }
    return value;
}

void kt_rw_property_set(KRef self, KRef property, KRef value) {
    if (self == NULL) {
        KT_FAIL("krusty: member access on a null receiver\n");
    }
    if (self->header.type != &kt_type_observable) {
        ((KNotNullVar *)self)->value = value;
        return;
    }
    KObservable *observable = (KObservable *)self;
    KRef old = observable->value;
    observable->value = value;
    /* AFTER the write, which is Kotlin's order: a callback reading the property sees the new
       value. `beforeChange` is `observable`'s own constant true, so there is nothing to veto. */
    (void)kt_invoke_three(observable->on_change, property, old, value);
}

/* ---- pairs --------------------------------------------------------------------------------- */

/* `a to b`. Two references and nothing else — Kotlin's `Pair` is a data class over two values, and
   every one of its members is one of the three `kotlin.Any` declares plus the two components. */
typedef struct KPair {
    KObjectHeader header;
    KRef first;
    KRef second;
} KPair;

static const uint32_t kt_pair_offsets[] = {offsetof(KPair, first), offsetof(KPair, second)};

static kt_boolean kt_pair_equals(KRef self, KRef other);
static kt_int kt_pair_hash_code(KRef self);
static KRef kt_pair_to_string(KRef self);

static const kt_fn kt_pair_vtable[] = {(kt_fn)kt_pair_equals, (kt_fn)kt_pair_hash_code,
                                       (kt_fn)kt_pair_to_string};

const KType kt_type_pair = {"kotlin.Pair",
                            sizeof("kotlin.Pair") - 1,
                            sizeof(KPair),
                            2,
                            0,
                            kt_pair_offsets,
                            &kt_type_any,
                            kt_pair_vtable,
                            3,
                            0};

KRef kt_pair_of(KRef first, KRef second) {
    /* Both components stay in these parameters across the allocation: they are its roots. */
    KPair *pair = (KPair *)kt_gc_allocate(&kt_type_pair, sizeof(KPair));
    pair->first = first;
    pair->second = second;
    return (KRef)pair;
}

KRef kt_pair_first(KRef pair) { return ((const KPair *)pair)->first; }

KRef kt_pair_second(KRef pair) { return ((const KPair *)pair)->second; }

/* A data class's `equals`: componentwise, and only against another `Pair`. */
static kt_boolean kt_pair_equals(KRef self, KRef other) {
    if (self == other) {
        return true;
    }
    if (other == NULL || other->header.type != &kt_type_pair) {
        return false;
    }
    const KPair *a = (const KPair *)self;
    const KPair *b = (const KPair *)other;
    return kt_equals(a->first, b->first) && kt_equals(a->second, b->second);
}

/* Kotlin's generated data-class hash: `first.hashCode() * 31 + second.hashCode()`, a null
   component contributing 0. */
static kt_int kt_pair_hash_code(KRef self) {
    const KPair *pair = (const KPair *)self;
    return (kt_int)(31u * (uint32_t)kt_hash_code(pair->first) + (uint32_t)kt_hash_code(pair->second));
}

/* `(first, second)` — `Pair` overrides the generated `toString` with this shape. */
static KRef kt_pair_to_string(KRef self) {
    const KPair *pair = (const KPair *)self;
    KRef text = kt_string_plus(kt_string_utf8("(", 1), kt_to_string(pair->first));
    text = kt_string_plus(text, kt_string_utf8(", ", 2));
    text = kt_string_plus(text, kt_to_string(pair->second));
    return kt_string_plus(text, kt_string_utf8(")", 1));
}

/* ---- ranges ---------------------------------------------------------------------------------- */

/* `1..3` as a value. The three closed integral ranges share one struct and one set of methods; the
   descriptor is what tells them apart, and it has to, because Kotlin's three declarations answer
   `equals`, `hashCode` and `toString` differently. An `IntRange` never equals a `LongRange` with
   the same bounds, which the type comparison in `kt_range_equals` is exactly. */

static kt_boolean kt_range_equals(KRef self, KRef other);
static kt_int kt_range_hash_code(KRef self);
static KRef kt_range_to_string(KRef self);

static const kt_fn kt_range_vtable[] = {(kt_fn)kt_range_equals, (kt_fn)kt_range_hash_code,
                                        (kt_fn)kt_range_to_string};

/* A range type or a range iterator: the shape fixes the instance size, and the table decides the
   three `kotlin.Any` members. A range answers all three by its bounds, as Kotlin's declarations do;
   an iterator is an ordinary object that answers by identity, as Kotlin's own iterators do. The
   fields are named rather than listed, so every field this does not mention is zero by rule and
   not by position: a positional list shifts when `KType` gains a field, and `-Wextra` rejects one
   that stops short of the last. */
#define KT_RANGE_TYPE(identifier, kotlin_name, shape, table)                                       \
    const KType identifier = {.name = kotlin_name,                                                 \
                              .name_length = sizeof(kotlin_name) - 1,                              \
                              .instance_size = sizeof(shape),                                      \
                              .super = &kt_type_any,                                               \
                              .vtable = table,                                                     \
                              .vtable_length = 3};

KT_RANGE_TYPE(kt_type_int_range, "kotlin.ranges.IntRange", KRange, kt_range_vtable)
KT_RANGE_TYPE(kt_type_long_range, "kotlin.ranges.LongRange", KRange, kt_range_vtable)
KT_RANGE_TYPE(kt_type_char_range, "kotlin.ranges.CharRange", KRange, kt_range_vtable)

/* The two UNSIGNED ranges. Kotlin declares only these two — `UByte.rangeTo` and `UShort.rangeTo`
   both answer a `UIntRange` — so the four unsigned scalars need no more than this pair. */
KT_RANGE_TYPE(kt_type_uint_range, "kotlin.ranges.UIntRange", KRange, kt_range_vtable)
KT_RANGE_TYPE(kt_type_ulong_range, "kotlin.ranges.ULongRange", KRange, kt_range_vtable)

/* Whether a range's bounds are to be READ unsigned. A `UIntRange`'s are stored zero-extended, so a
   signed 64-bit comparison of them happens to answer correctly; a `ULongRange`'s occupy all 64
   bits, where `18446744073709551615uL` is `-1` read signed and every comparison below would be
   wrong. One flag serves both rather than resting on that coincidence for the narrower one. */
static kt_boolean kt_range_unsigned(const KType *type) {
    return type == &kt_type_uint_range || type == &kt_type_ulong_range;
}

/* `a < b` at the width and signedness the range reads its bounds with. */
static kt_boolean kt_range_below(kt_boolean unsigned_bounds, kt_long a, kt_long b) {
    return unsigned_bounds ? (uint64_t)a < (uint64_t)b : a < b;
}

/* `a` reduced modulo `c` into `0 until c`, for `c > 0`. C's `%` answers the sign of `a`, which is
   the wrong half of the line for the difference below. */
static kt_long kt_floor_mod(kt_long a, kt_long c) {
    kt_long remainder = a % c;
    return remainder < 0 ? remainder + c : remainder;
}

/* How far `a` overshoots the last point of the walk `b` begins, stepping by `c > 0`: the distance
   between the two ends modulo the step, taken on the ring rather than on the line. */
static kt_long kt_difference_modulo(kt_long a, kt_long b, kt_long c) {
    kt_long a_mod = kt_floor_mod(a, c);
    kt_long b_mod = kt_floor_mod(b, c);
    return a_mod >= b_mod ? a_mod - b_mod : a_mod - b_mod + c;
}

/* The same two questions on the unsigned ring. `%` on a `uint64_t` already lands in `0 until c`,
   so there is no negative remainder to fold back — and folding one the signed way is exactly what
   goes wrong above 2^63, where a `ULong` bound reads as a negative `kt_long` and `%` answers the
   wrong half of the ring. Kotlin keeps unsigned overloads of `getProgressionLastElement` for this
   reason rather than reusing the signed ones. */
static uint64_t kt_difference_modulo_unsigned(uint64_t a, uint64_t b, uint64_t c) {
    uint64_t a_mod = a % c;
    uint64_t b_mod = b % c;
    return a_mod >= b_mod ? a_mod - b_mod : a_mod - b_mod + c;
}

/* The last value a progression actually reaches: the bound written, pulled back to the nearest
   point on the step.

   The obvious `first + ((last - first) / step) * step` is right for every range a program is
   likely to write and wrong for the ones the corpus asks about, because `last - first` is a
   DISTANCE and the widest ones do not fit: `Long.MIN_VALUE..Long.MAX_VALUE` spans more than a
   `Long` can hold, so the subtraction wraps and the walk stops after one element. Working modulo
   the step instead never forms that distance — every intermediate here is inside `0 until step` —
   which is why Kotlin's own `getProgressionLastElement` is written this way too. */
static kt_long kt_range_last_element(kt_boolean unsigned_bounds, kt_long first, kt_long last,
                                     kt_long step) {
    if (step > 0 ? kt_range_below(unsigned_bounds, last, first)
                 : kt_range_below(unsigned_bounds, first, last)) {
        /* Empty. Kotlin keeps the bounds as written and answers `isEmpty`, rather than inventing a
           last element that the walk would then have to avoid. */
        return last;
    }
    if (step > 0) {
        if (unsigned_bounds) {
            return (kt_long)((uint64_t)last
                             - kt_difference_modulo_unsigned((uint64_t)last, (uint64_t)first,
                                                             (uint64_t)step));
        }
        return last - kt_difference_modulo(last, first, step);
    }
    if (unsigned_bounds) {
        return (kt_long)((uint64_t)last
                         + kt_difference_modulo_unsigned((uint64_t)first, (uint64_t)last,
                                                         (uint64_t)-step));
    }
    /* Descending: the same question about the walk running the other way, so the roles of the two
       bounds swap and the step is taken by magnitude. `-step` cannot overflow — a step of
       `Long.MIN_VALUE` is not reachable, since every step is either a literal magnitude or the
       negation of one. */
    return last + kt_difference_modulo(first, last, -step);
}

static KRef kt_range_allocate(const KType *type, kt_long first, kt_long last, kt_long step,
                              kt_boolean progression) {
    KRange *range = (KRange *)kt_gc_allocate(type, sizeof(KRange));
    range->first = first;
    range->last = kt_range_last_element(kt_range_unsigned(type), first, last, step);
    range->step = step;
    range->progression = progression;
    return (KRef)range;
}

/* A progression: what `step`, `downTo` and `reversed()` answer. It is one even when its step is
   `1` — `1..3 step 1` renders `1..3 step 1` — which is why the flag is kept rather than read off
   the step. */
static KRef kt_range_new_stepped(const KType *type, kt_long first, kt_long last, kt_long step) {
    return kt_range_allocate(type, first, last, step, true);
}

/* A range: what `..` and `until` answer. */
static KRef kt_range_new(const KType *type, kt_long first, kt_long last) {
    return kt_range_allocate(type, first, last, 1, false);
}

KRef kt_int_range(kt_int first, kt_int last) {
    return kt_range_new(&kt_type_int_range, first, last);
}

KRef kt_long_range(kt_long first, kt_long last) {
    return kt_range_new(&kt_type_long_range, first, last);
}

KRef kt_char_range(kt_char first, kt_char last) {
    return kt_range_new(&kt_type_char_range, first, last);
}

/* The unsigned pair. Both take their bounds already ZERO-EXTENDED into 64 bits, which is what the
   generator's own widening produces and what keeps a `UInt` bound from arriving sign-extended and
   comparing below zero. */
KRef kt_uint_range(kt_int first, kt_int last) {
    return kt_range_new(&kt_type_uint_range, (uint32_t)first, (uint32_t)last);
}

KRef kt_ulong_range(kt_long first, kt_long last) {
    return kt_range_new(&kt_type_ulong_range, first, last);
}

/* The half-open form. Kotlin's `until` answers the declared EMPTY range when `last` is the element
   type's minimum, rather than computing `last - 1` and wrapping round to the maximum. That empty
   range is a specific pair of bounds -- `1..0` for the integral types, and the same pair widened
   for `Char` -- so an empty range from `until` prints and hashes as that one. */
KRef kt_int_range_until(kt_int first, kt_int last) {
    if (last == INT32_MIN) {
        return kt_int_range(1, 0);
    }
    return kt_int_range(first, last - 1);
}

KRef kt_long_range_until(kt_long first, kt_long last) {
    if (last == INT64_MIN) {
        return kt_long_range(1, 0);
    }
    return kt_long_range(first, last - 1);
}

KRef kt_char_range_until(kt_char first, kt_char last) {
    if (last == 0) {
        return kt_char_range(1, 0);
    }
    return kt_char_range(first, (kt_char)(last - 1));
}

/* The unsigned half-open forms. The minimum an unsigned type wraps at is ZERO, not its signed
   minimum, so that is the bound `until` answers the empty range for. And the empty range answered
   is not the signed types' `1..0`: `UIntRange.EMPTY` is `UInt.MAX_VALUE..UInt.MIN_VALUE`, and
   `ULongRange.EMPTY` the same pair at 64 bits, which `first` and `toString` both show. */
KRef kt_uint_range_until(kt_int first, kt_int last) {
    if ((uint32_t)last == 0) {
        return kt_uint_range((kt_int)UINT32_MAX, 0);
    }
    return kt_uint_range(first, (kt_int)((uint32_t)last - 1));
}

KRef kt_ulong_range_until(kt_long first, kt_long last) {
    if ((uint64_t)last == 0) {
        return kt_ulong_range((kt_long)UINT64_MAX, 0);
    }
    return kt_ulong_range(first, (kt_long)((uint64_t)last - 1));
}

/* Empty is direction-dependent once there is a step: `10 downTo 1` runs, `1 downTo 10` does not. */
static kt_boolean kt_range_empty(const KRange *range) {
    kt_boolean unsigned_bounds = kt_range_unsigned(range->header.type);
    return range->step > 0 ? kt_range_below(unsigned_bounds, range->last, range->first)
                           : kt_range_below(unsigned_bounds, range->first, range->last);
}

kt_boolean kt_range_is_empty(KRef range) { return kt_range_empty((const KRange *)range); }

kt_long kt_range_first(KRef range) { return ((const KRange *)range)->first; }

kt_long kt_range_last(KRef range) { return ((const KRange *)range)->last; }

/* Membership in a range OR a progression, which Kotlin answers by walking and this answers in
   constant time with the same result.

   Two conditions, and a plain range satisfies the second for free. The value must lie between the
   ends IN THE WALK'S OWN DIRECTION -- a descending progression has `first` above `last`, so the
   ascending test would reject every member of it -- and it must sit ON the step: `5 in
   (10 downTo 1 step 2)` is false though 5 lies between 10 and 2, because the walk visits only
   10, 8, 6, 4, 2.

   `last` is already the last element REACHED (see `KRange`), so an EMPTY range fails the bounds
   test for every value and needs no case of its own. A plain range steps by 1, where the
   step test is always true.

   The bounds were stored at 64 bits with the element type's own signedness, and the caller widens
   the value the same way, so both tests read all five integral types once they read the unsigned
   two as unsigned. */
kt_boolean kt_range_contains(KRef range, kt_long value) {
    const KRange *self = (const KRange *)range;
    kt_boolean unsigned_bounds = kt_range_unsigned(range->header.type);
    kt_long step = self->step;
    if (step > 0) {
        if (kt_range_below(unsigned_bounds, value, self->first)
            || kt_range_below(unsigned_bounds, self->last, value)) {
            return false;
        }
    } else {
        if (kt_range_below(unsigned_bounds, self->first, value)
            || kt_range_below(unsigned_bounds, value, self->last)) {
            return false;
        }
    }
    /* Taken on the ring rather than the line, so an ascending walk and a descending one are the
       same expression: the value is reached iff it is congruent to `first` modulo the step. On the
       UNSIGNED ring for the unsigned two, as `kt_range_last_element` does it: a `ULong` above 2^63
       reads as a negative `kt_long`, whose signed residue is a different number, so
       `9223372036854775809uL in (0uL..ULong.MAX_VALUE step 3)` would answer false. */
    kt_long magnitude = step > 0 ? step : -step;
    if (unsigned_bounds) {
        return kt_difference_modulo_unsigned((uint64_t)value, (uint64_t)self->first,
                                             (uint64_t)magnitude)
               == 0;
    }
    return kt_difference_modulo(value, self->first, magnitude) == 0;
}

/* Two are equal when both are empty, or when both bounds match -- and only within one element
   type: `1..3` is an `IntRange` and never equals the `LongRange` of the same bounds. A progression
   compares its step as well, so `10 downTo 1` is not `10..1`, and `1..9 step 2` is not
   `1..9 step 4`.

   The two classes do not answer each other symmetrically, and Kotlin's do not either: `IntRange`
   subclasses `IntProgression`, so a progression's `equals` accepts a range (`(1..3 step 1) ==
   (1..3)` is true), while a range's accepts only a range (`(1..3) == (1..3 step 1)` is false). */
static kt_boolean kt_range_equals(KRef self, KRef other) {
    if (other == NULL || other->header.type != self->header.type) {
        return false;
    }
    const KRange *a = (const KRange *)self;
    const KRange *b = (const KRange *)other;
    if (!a->progression && b->progression) {
        return false;
    }
    if (kt_range_empty(a) && kt_range_empty(b)) {
        return true;
    }
    return a->first == b->first && a->last == b->last && (!a->progression || a->step == b->step);
}

/* A 64-bit bound or step folded the way `Long.hashCode` folds it: the two halves xored together.
   Only the low 32 bits of any sum of these matter, and those depend only on the low 32 bits of
   each part, so folding first and summing at 32 bits answers what Kotlin's `Long` sum and
   `toInt()` do. */
static uint32_t kt_range_fold(kt_long value) {
    return (uint32_t)((uint64_t)value ^ ((uint64_t)value >> 32));
}

/* Kotlin's own: `-1` for an empty one; `31 * first + last` for a range, and
   `31 * (31 * first + last) + step` for a progression; each part folded through its own type's
   `hashCode` first, which for `Long` and `ULong` is `kt_range_fold` and for the narrower types is
   the value itself. */
static kt_int kt_range_hash_code(KRef self) {
    const KRange *range = (const KRange *)self;
    if (kt_range_empty(range)) {
        return -1;
    }
    kt_boolean wide =
        self->header.type == &kt_type_long_range || self->header.type == &kt_type_ulong_range;
    uint32_t first = wide ? kt_range_fold(range->first) : (uint32_t)range->first;
    uint32_t last = wide ? kt_range_fold(range->last) : (uint32_t)range->last;
    uint32_t hash = 31u * first + last;
    if (range->progression) {
        hash = 31u * hash + (wide ? kt_range_fold(range->step) : (uint32_t)range->step);
    }
    return (kt_int)hash;
}

/* A range's iterator: the bounds again, plus the one bit that makes the LAST step terminate
   without overflowing. `for (i in 1..Int.MAX_VALUE)` is why the bit exists — incrementing past the
   maximum wraps, and a `next <= last` test would then never stop. Kotlin's own
   `IntProgressionIterator` carries the same flag for the same reason. */
typedef struct KRangeIterator {
    KObjectHeader header;
    kt_long next;
    kt_long last;
    kt_long step;
    kt_boolean has_next;
} KRangeIterator;

KT_RANGE_TYPE(kt_type_int_iterator, "kotlin.collections.IntIterator", KRangeIterator, kt_any_vtable)
KT_RANGE_TYPE(kt_type_long_iterator, "kotlin.collections.LongIterator", KRangeIterator, kt_any_vtable)
KT_RANGE_TYPE(kt_type_char_iterator, "kotlin.collections.CharIterator", KRangeIterator, kt_any_vtable)
KT_RANGE_TYPE(kt_type_uint_iterator, "kotlin.collections.UIntIterator", KRangeIterator, kt_any_vtable)
KT_RANGE_TYPE(kt_type_ulong_iterator, "kotlin.collections.ULongIterator", KRangeIterator, kt_any_vtable)

/* Whether this is one of the RANGES this runtime makes. Asked before a range's struct is read
   through, because everything that is not one of this runtime's own shapes and not a walkable
   class of the program ends up at the range reader — and a value read as a struct it is not is a
   wrong answer where a refusal is the honest one. */
static kt_boolean kt_is_range(KRef value) {
    return value != NULL
           && (value->header.type == &kt_type_int_range || value->header.type == &kt_type_long_range
               || value->header.type == &kt_type_char_range
               || value->header.type == &kt_type_uint_range
               || value->header.type == &kt_type_ulong_range);
}

/* The same for the iterator a range hands out. */
static kt_boolean kt_is_range_iterator(KRef value) {
    return value != NULL
           && (value->header.type == &kt_type_int_iterator
               || value->header.type == &kt_type_long_iterator
               || value->header.type == &kt_type_char_iterator
               || value->header.type == &kt_type_uint_iterator
               || value->header.type == &kt_type_ulong_iterator);
}

KRef kt_range_iterator(KRef range) {
    if (!kt_is_range(range)) {
        KT_FAIL("krusty: this value cannot be walked\n");
    }
    const KRange *bounds = (const KRange *)range;
    const KType *type = range->header.type == &kt_type_long_range    ? &kt_type_long_iterator
                        : range->header.type == &kt_type_char_range  ? &kt_type_char_iterator
                        : range->header.type == &kt_type_uint_range  ? &kt_type_uint_iterator
                        : range->header.type == &kt_type_ulong_range ? &kt_type_ulong_iterator
                                                                     : &kt_type_int_iterator;
    KRangeIterator *iterator = (KRangeIterator *)kt_gc_allocate(type, sizeof(KRangeIterator));
    iterator->next = bounds->first;
    iterator->last = bounds->last;
    iterator->step = bounds->step;
    iterator->has_next = !kt_range_empty(bounds);
    return (KRef)iterator;
}

/* `range step n`. The magnitude is what is given; the receiver's direction is kept, which is why
   `10 downTo 1 step 3` descends. A step of zero has no walk to describe and is Kotlin's
   `IllegalArgumentException`. */
KRef kt_range_step(KRef range, kt_long step) {
    const KRange *bounds = (const KRange *)range;
    if (step <= 0) {
        /* Kotlin's own message, which names the step that was given. The RETURN matters: the
           exception is recorded, not raised, so falling through would build a progression whose
           step is zero and whose last element is computed modulo it — a SIGFPE, and a machine
           trap where Kotlin has an exception a program is entitled to catch. */
        KRef message = kt_string_plus(kt_string_utf8("Step must be positive, was: ", 28),
                                      kt_to_string(kt_box_long(step)));
        message = kt_string_plus(message, kt_string_utf8(".", 1));
        kt_throw(kt_throwable_new(&kt_type_illegal_argument_exception, message));
        return range;
    }
    return kt_range_new_stepped(range->header.type, bounds->first, bounds->last,
                                bounds->step > 0 ? step : -step);
}

/* `reversed()`. The walk runs the other way from the LAST ELEMENT, which is already on the step —
   so `(1..9 step 3).reversed()` is `7 downTo 1 step 3`, not `9 downTo 1 step 3`. */
KRef kt_range_reversed(KRef range) {
    const KRange *bounds = (const KRange *)range;
    return kt_range_new_stepped(range->header.type, bounds->last, bounds->first, -bounds->step);
}

KRef kt_int_range_down_to(kt_int first, kt_int last) {
    return kt_range_new_stepped(&kt_type_int_range, first, last, -1);
}

KRef kt_long_range_down_to(kt_long first, kt_long last) {
    return kt_range_new_stepped(&kt_type_long_range, first, last, -1);
}

KRef kt_char_range_down_to(kt_char first, kt_char last) {
    return kt_range_new_stepped(&kt_type_char_range, first, last, -1);
}

/* The unsigned descending forms. Their bounds arrive already ZERO-EXTENDED, like every other
   unsigned bound here, so `UInt.MAX_VALUE downTo 0u` keeps a first bound above the signed maximum
   instead of reading as -1 and answering empty. */
KRef kt_uint_range_down_to(kt_int first, kt_int last) {
    return kt_range_new_stepped(&kt_type_uint_range, (uint32_t)first, (uint32_t)last, -1);
}

KRef kt_ulong_range_down_to(kt_long first, kt_long last) {
    return kt_range_new_stepped(&kt_type_ulong_range, first, last, -1);
}

/* Defined with the array and string walks below. A program that asks a primitive array for an
   iterator is handed one of those, and its STATIC type is `IntIterator`/`LongIterator`/
   `CharIterator` — the narrow protocol these two functions implement. So the walk has to answer
   here as well as through the general dispatch, and the descriptor is what says which object this
   is. */
static kt_boolean kt_walk_is(KRef iterator);
static kt_boolean kt_walk_has_next(KRef iterator);
static kt_long kt_walk_next_long(KRef iterator);

kt_boolean kt_range_iterator_has_next(KRef iterator) {
    if (kt_walk_is(iterator)) {
        return kt_walk_has_next(iterator);
    }
    if (!kt_is_range_iterator(iterator)) {
        KT_FAIL("krusty: this value is no iterator\n");
    }
    return ((const KRangeIterator *)iterator)->has_next;
}

kt_long kt_range_iterator_next(KRef iterator) {
    if (kt_walk_is(iterator)) {
        return kt_walk_next_long(iterator);
    }
    if (!kt_is_range_iterator(iterator)) {
        KT_FAIL("krusty: this value is no iterator\n");
    }
    KRangeIterator *self = (KRangeIterator *)iterator;
    if (!self->has_next) {
        KT_FAIL("krusty: no more elements in this range\n");
    }
    kt_long value = self->next;
    /* Stopping at the LAST ELEMENT rather than by comparing against the bound is what keeps this
       correct at the extremes: `last` is already on the step, so one more step from it may
       overflow, and asking whether the NEXT value is past the end would have to compute it first. */
    if (value == self->last) {
        self->has_next = false;
    } else {
        /* Added on the unsigned ring, which is exact for every type here: the result is the next
           element, which the walk reaches and so fits. Only the arithmetic has to be told so — a
           `ULong` walk from `Long.MAX_VALUE` to the next value is a SIGNED overflow in `kt_long`,
           undefined behaviour, however right the bits it usually produces. */
        self->next = (kt_long)((uint64_t)value + (uint64_t)self->step);
    }
    return value;
}


/* ---- floating-point ranges ------------------------------------------------------------------ */

/* `0.0..2.0`. A floating-point range is NOT a progression: it has no step and no walk, because
   there is no next floating-point number for Kotlin to name. It is a pair of bounds and the
   question `value in it`, which is why it gets a shape of its own rather than joining `KRange` —
   whose bounds are 64-bit integers read at the element's signedness, and a `Double`'s bits are not
   an integer's.

   A `Float` range is stored at `double`. Widening a float is exact and order-preserving, and the
   value tested widens the same way, so every comparison answers what float comparison would; the
   descriptor is what remembers which it is, for rendering and for equality.

   NaN is worth spelling out and is not a special case anywhere below. `contains` is
   `value >= start && value <= end`, so a NaN on either side answers false; `isEmpty` is
   `!(start <= end)`, so a range with a NaN bound is empty. Both fall out of IEEE comparison, which
   is what Kotlin's own `lessThanOrEquals` on these types is. */
/* Defined with the rest of the value hashing further down: the bits `equals` and `hashCode` read
   from a floating-point value, with every NaN collapsed to one. */
static uint64_t kt_double_bits(kt_double value);
static uint32_t kt_float_bits(kt_float value);

typedef struct KFloatingRange {
    KObjectHeader header;
    kt_double start;
    kt_double end;
} KFloatingRange;

static kt_boolean kt_floating_range_equals(KRef self, KRef other);
static kt_int kt_floating_range_hash_code(KRef self);
static KRef kt_floating_range_to_string(KRef self);

static const kt_fn kt_floating_range_vtable[] = {(kt_fn)kt_floating_range_equals,
                                                 (kt_fn)kt_floating_range_hash_code,
                                                 (kt_fn)kt_floating_range_to_string};

/* Kotlin's own names for these: the class a `rangeTo` on a floating-point receiver answers with is
   private to the stdlib, and `ClosedFloatingPointRange` is the interface it is seen through. The
   name here is the CLASS's, because it is what `toString` on the object would report. */
KT_RANGE_TYPE(kt_type_double_range, "kotlin.ranges.ClosedDoubleRange", KFloatingRange,
              kt_floating_range_vtable)
KT_RANGE_TYPE(kt_type_float_range, "kotlin.ranges.ClosedFloatRange", KFloatingRange,
              kt_floating_range_vtable)

static KRef kt_floating_range_new(const KType *type, kt_double start, kt_double end) {
    KFloatingRange *range =
        (KFloatingRange *)kt_gc_allocate(type, sizeof(KFloatingRange));
    range->start = start;
    range->end = end;
    return (KRef)range;
}

KRef kt_double_range(kt_double start, kt_double end) {
    return kt_floating_range_new(&kt_type_double_range, start, end);
}

KRef kt_float_range(kt_float start, kt_float end) {
    return kt_floating_range_new(&kt_type_float_range, (kt_double)start, (kt_double)end);
}

kt_boolean kt_floating_range_is_empty(KRef range) {
    const KFloatingRange *self = (const KFloatingRange *)range;
    return !(self->start <= self->end);
}

kt_boolean kt_floating_range_contains(KRef range, kt_double value) {
    const KFloatingRange *self = (const KFloatingRange *)range;
    return value >= self->start && value <= self->end;
}

kt_double kt_floating_range_start(KRef range) {
    return ((const KFloatingRange *)range)->start;
}

kt_double kt_floating_range_end(KRef range) { return ((const KFloatingRange *)range)->end; }

/* Two are equal when both are EMPTY, or when both bounds are equal by IEEE comparison — Kotlin's
   own, and the empty case is what makes `NaN..NaN` equal itself despite `NaN != NaN`. Only within
   one range type: a `Float` range never equals a `Double` one. */
static kt_boolean kt_floating_range_equals(KRef self, KRef other) {
    if (other == NULL || other->header.type != self->header.type) {
        return false;
    }
    const KFloatingRange *a = (const KFloatingRange *)self;
    const KFloatingRange *b = (const KFloatingRange *)other;
    if (kt_floating_range_is_empty(self) && kt_floating_range_is_empty(other)) {
        return true;
    }
    return a->start == b->start && a->end == b->end;
}

/* Kotlin's own: `-1` for an empty range, else `31 * start.hashCode() + end.hashCode()`, each bound
   folded through the hash of its OWN type — so a `Float` range hashes its bounds at 32 bits. */
static kt_int kt_floating_range_hash_code(KRef self) {
    const KFloatingRange *range = (const KFloatingRange *)self;
    if (kt_floating_range_is_empty(self)) {
        return -1;
    }
    if (self->header.type == &kt_type_float_range) {
        uint32_t start = kt_float_bits((kt_float)range->start);
        uint32_t end = kt_float_bits((kt_float)range->end);
        return (kt_int)(31u * start + end);
    }
    uint64_t start = kt_double_bits(range->start);
    uint64_t end = kt_double_bits(range->end);
    kt_int first = (kt_int)(uint32_t)(start ^ (start >> 32));
    kt_int last = (kt_int)(uint32_t)(end ^ (end >> 32));
    return (kt_int)(31u * (uint32_t)first + (uint32_t)last);
}

/* `"$start..$end"`, each bound rendered as its own type would render it. */
static KRef kt_floating_range_to_string(KRef self) {
    const KFloatingRange *range = (const KFloatingRange *)self;
    kt_boolean single = self->header.type == &kt_type_float_range;
    KRef start = single ? kt_box_float((kt_float)range->start) : kt_box_double(range->start);
    KRef end = single ? kt_box_float((kt_float)range->end) : kt_box_double(range->end);
    KRef text = kt_string_plus(kt_to_string(start), kt_string_utf8("..", 2));
    return kt_string_plus(text, kt_to_string(end));
}

#undef KT_RANGE_TYPE

/* ---- comparable ranges ----------------------------------------------------------------------

   `"a".."c"`, and every other `a..b` whose bounds are ordered by `Comparable` rather than by a
   machine comparison. Kotlin's `rangeTo` for those answers a `ComparableRange<T>`, seen through
   `ClosedRange<T>`; it holds the two bounds as OBJECTS and asks each one how it compares, which is
   exactly what `kt_compare_any` does here.

   No walk, for the reason a floating-point range has none: `Comparable` names no successor, so
   there is nothing to step by. A pair of bounds and the question `value in it`. */
typedef struct KComparableRange {
    KObjectHeader header;
    KRef start;
    KRef end;
} KComparableRange;

static const uint32_t kt_comparable_range_offsets[] = {offsetof(KComparableRange, start),
                                                       offsetof(KComparableRange, end)};

static kt_boolean kt_comparable_range_equals(KRef self, KRef other);
static kt_int kt_comparable_range_hash_code(KRef self);
static KRef kt_comparable_range_to_string(KRef self);

static const kt_fn kt_comparable_range_vtable[] = {(kt_fn)kt_comparable_range_equals,
                                                   (kt_fn)kt_comparable_range_hash_code,
                                                   (kt_fn)kt_comparable_range_to_string};

const KType kt_type_comparable_range = {
    .name = "kotlin.ranges.ComparableRange",
    .name_length = sizeof("kotlin.ranges.ComparableRange") - 1,
    .instance_size = sizeof(KComparableRange),
    .reference_count = 2,
    .reference_offsets = kt_comparable_range_offsets,
    .super = &kt_type_any,
    .vtable = kt_comparable_range_vtable,
    .vtable_length = 3,
};

KRef kt_comparable_range(KRef start, KRef end) {
    KComparableRange *range =
        (KComparableRange *)kt_gc_allocate(&kt_type_comparable_range, sizeof(KComparableRange));
    range->start = start;
    range->end = end;
    return (KRef)range;
}

/* `start > end`, which is Kotlin's own `isEmpty` for this class. */
kt_boolean kt_comparable_range_is_empty(KRef range) {
    const KComparableRange *self = (const KComparableRange *)range;
    return kt_compare_any(self->start, self->end) > 0;
}

/* `value >= start && value <= end`, each comparison the VALUE's own. Kotlin's `ComparableRange`
   asks the same way round, which matters for a `compareTo` that is not symmetric. */
kt_boolean kt_comparable_range_contains(KRef range, KRef value) {
    const KComparableRange *self = (const KComparableRange *)range;
    return kt_compare_any(value, self->start) >= 0 && kt_compare_any(value, self->end) <= 0;
}

KRef kt_comparable_range_start(KRef range) { return ((const KComparableRange *)range)->start; }

KRef kt_comparable_range_end(KRef range) { return ((const KComparableRange *)range)->end; }

/* Kotlin's own three, which are `ClosedRange`'s documented contract: two empty ranges are equal
   whatever their bounds, and `-1` hashes an empty one. `toString` is `"$start..$endInclusive"`
   whether or not the range is empty, as `ComparableRange`'s own is. */
static kt_boolean kt_comparable_range_equals(KRef self, KRef other) {
    if (other == NULL || other->header.type != &kt_type_comparable_range) {
        return false;
    }
    if (kt_comparable_range_is_empty(self) && kt_comparable_range_is_empty(other)) {
        return true;
    }
    const KComparableRange *a = (const KComparableRange *)self;
    const KComparableRange *b = (const KComparableRange *)other;
    return kt_equals(a->start, b->start) && kt_equals(a->end, b->end);
}

static kt_int kt_comparable_range_hash_code(KRef self) {
    if (kt_comparable_range_is_empty(self)) {
        return -1;
    }
    const KComparableRange *range = (const KComparableRange *)self;
    return (kt_int)(31u * (uint32_t)kt_hash_code(range->start) +
                    (uint32_t)kt_hash_code(range->end));
}

static KRef kt_comparable_range_to_string(KRef self) {
    const KComparableRange *range = (const KComparableRange *)self;
    KRef text = kt_string_plus(kt_to_string(range->start), kt_string_utf8("..", 2));
    return kt_string_plus(text, kt_to_string(range->end));
}


/* Append `count` bytes of `text` at `out`, answering how many were written. */
static kt_int kt_range_put(char *out, const char *text, kt_int count) {
    memcpy(out, text, (size_t)count);
    return count;
}

/* `"$first..$last"` for a range. A progression names its step as well, and a descending one is
   written the way it is built: `"$first..$last step $step"`, or `"$first downTo $last step
   ${-step}"`. A `CharRange`'s bounds render as the characters they are and an unsigned range's
   as unsigned numbers; the step is a signed `Int` or `Long` whatever the element type. */
static KRef kt_range_to_string(KRef self) {
    const KRange *range = (const KRange *)self;
    kt_boolean chars = self->header.type == &kt_type_char_range;
    kt_boolean unsigned_bounds = kt_range_unsigned(self->header.type);
    kt_boolean descending = range->progression && range->step < 0;
    /* Two 20-digit bounds, " downTo " and " step ", and a 19-digit step. */
    KByteArray *buffer = kt_bytes_new(80);
    char *out = kt_bytes_of(buffer);
    kt_int length = chars ? kt_render_char((kt_char)range->first, out)
                    : unsigned_bounds ? kt_render_ulong((uint64_t)range->first, out)
                                      : kt_render_long(range->first, out);
    length += descending ? kt_range_put(out + length, " downTo ", 8)
                         : kt_range_put(out + length, "..", 2);
    length += chars ? kt_render_char((kt_char)range->last, out + length)
              : unsigned_bounds ? kt_render_ulong((uint64_t)range->last, out + length)
                                : kt_render_long(range->last, out + length);
    if (range->progression) {
        length += kt_range_put(out + length, " step ", 6);
        /* The magnitude: a step is never `Long.MIN_VALUE` (see `kt_range_last_element`), so the
           negation cannot overflow. */
        length += kt_render_long(descending ? -range->step : range->step, out + length);
    }
    return kt_string_of((KRef)buffer, kt_bytes_of(buffer), length);
}

/* Copy `source`'s elements into `destination` starting at `at`, and answer where the next element
   goes. The two arrays share an element kind — a spread into a `vararg` is a spread of the same
   type — so the DESTINATION's stride measures both.

   This exists because a spread's length is only known at run time: `f(a, *xs, b)` builds one array
   whose size nothing static can compute, and copying is what the callee's own array must contain.
   A copy, rather than passing `xs` itself, is also what keeps the callee from writing through to
   the caller's array. */
kt_int kt_array_copy_into(KRef destination, kt_int at, KRef source) {
    if (destination == NULL || source == NULL) {
        KT_FAIL("krusty: a spread of null\n");
    }
    kt_int length = ((const KArray *)source)->length;
    kt_int capacity = ((const KArray *)destination)->length;
    uint32_t stride = destination->header.type->element_size;
    /* The end is taken at 64 bits: `at + length` in `kt_int` overflows for a start near
       `Int.MAX_VALUE`, which is undefined behaviour and in practice wraps negative and passes. */
    if (at < 0 || length < 0 || (kt_long)at + length > capacity) {
        /* The index reported is the first one the copy would write outside the array: the start
           when that is below it, else the first past its end. The RETURN matters: the exception is
           recorded, not raised, so falling through would copy past the destination's end and over
           whatever the heap holds next, before anything reads the exception. */
        kt_index_out_of_bounds(at < 0 ? at : capacity, capacity);
        return at;
    }
    memcpy((char *)(KArray *)destination + sizeof(KArray) + (size_t)at * stride,
           (const char *)(const KArray *)source + sizeof(KArray), (size_t)length * stride);
    return at + length;
}

