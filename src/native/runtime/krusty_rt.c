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

