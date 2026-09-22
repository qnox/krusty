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

/* The last dot-separated segment of the qualified name. A name with no dot is its own simple name,
   which is what a class in the root package has. */
KRef kt_class_simple_name(KRef self) {
    const KType *described = ((const KClass *)self)->described;
    kt_int start = 0;
    for (kt_int at = 0; at < (kt_int)described->name_length; at++) {
        if (described->name[at] == '.') {
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
   an iterator is an ordinary object that answers by identity, as Kotlin's own iterators do. */
#define KT_RANGE_TYPE(identifier, kotlin_name, shape, table)                                       \
    const KType identifier = {kotlin_name, sizeof(kotlin_name) - 1, sizeof(shape), 0,              \
                              0,           NULL,                    &kt_type_any,                  \
                              table, 3, 0};

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


/* The last element a walk from `first` by `step` actually reaches, which is what `last` holds; see
   the note on `KRange`. Both differences are taken at 64 bits, and the quotient truncates toward
   zero, which is what makes one expression serve an ascending and a descending walk: the numerator
   and the step always share a sign here, so the count is non-negative either way. */
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

static KRef kt_range_new_stepped(const KType *type, kt_long first, kt_long last, kt_long step) {
    KRange *range = (KRange *)kt_gc_allocate(type, sizeof(KRange));
    range->first = first;
    range->last = kt_range_last_element(kt_range_unsigned(type), first, last, step);
    range->step = step;
    return (KRef)range;
}

static KRef kt_range_new(const KType *type, kt_long first, kt_long last) {
    return kt_range_new_stepped(type, first, last, 1);
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
   minimum, so that is the bound `until` answers the empty range for. */
KRef kt_uint_range_until(kt_int first, kt_int last) {
    if ((uint32_t)last == 0) {
        return kt_uint_range(1, 0);
    }
    return kt_uint_range(first, (kt_int)((uint32_t)last - 1));
}

KRef kt_ulong_range_until(kt_long first, kt_long last) {
    if ((uint64_t)last == 0) {
        return kt_ulong_range(1, 0);
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

/* `value in range`. The bounds were stored at 64 bits with the element type's own signedness, so
   the caller widens the same way and one comparison serves all three. */
/* Membership in a range OR a progression, which Kotlin answers by walking and this answers in
   constant time with the same result.

   Two conditions, and a plain range satisfies the second for free. The value must lie between the
   ends IN THE WALK'S OWN DIRECTION -- a descending progression has `first` above `last`, so the
   ascending test would reject every member of it -- and it must sit ON the step: `5 in
   (10 downTo 1 step 2)` is false though 5 lies between 10 and 2, because the walk visits only
   10, 8, 6, 4, 2.

   `last` is already the last element REACHED (see `KRange`), so an EMPTY range fails the bounds
   test for every value and needs no case of its own. A plain range steps by 1, where the
   step test is always true. */
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
    /* Taken on the ring rather than the line, so an unsigned walk and a descending one are the
       same expression: the value is reached iff it is congruent to `first` modulo the step. */
    return kt_difference_modulo(value, self->first, step > 0 ? step : -step) == 0;
}

/* Two ranges are equal when both are empty, or when both bounds match -- and only within one range
   type: `1..3` is an `IntRange` and never equals the `LongRange` of the same bounds. */
static kt_boolean kt_range_equals(KRef self, KRef other) {
    if (other == NULL || other->header.type != self->header.type) {
        return false;
    }
    const KRange *a = (const KRange *)self;
    const KRange *b = (const KRange *)other;
    if (kt_range_empty(a) && kt_range_empty(b)) {
        return true;
    }
    return a->first == b->first && a->last == b->last;
}

/* Kotlin's own: `-1` for an empty range, else `31 * first + last`, with each bound folded through
   its own `hashCode` first -- which for a `Long` is the two halves xored together. */
static kt_int kt_range_hash_code(KRef self) {
    const KRange *range = (const KRange *)self;
    if (kt_range_empty(range)) {
        return -1;
    }
    if (self->header.type == &kt_type_long_range || self->header.type == &kt_type_ulong_range) {
        kt_int first = (kt_int)(range->first ^ (kt_long)((uint64_t)range->first >> 32));
        kt_int last = (kt_int)(range->last ^ (kt_long)((uint64_t)range->last >> 32));
        return (kt_int)(31u * (uint32_t)first + (uint32_t)last);
    }
    return (kt_int)(31u * (uint32_t)range->first + (uint32_t)range->last);
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

KRef kt_range_iterator(KRef range) {
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
    return ((const KRangeIterator *)iterator)->has_next;
}

kt_long kt_range_iterator_next(KRef iterator) {
    if (kt_walk_is(iterator)) {
        return kt_walk_next_long(iterator);
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
        self->next = value + self->step;
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

/* `"$first..$last"`, with a `CharRange`'s bounds rendered as the characters they are. */
static KRef kt_range_to_string(KRef self) {
    const KRange *range = (const KRange *)self;
    kt_boolean chars = self->header.type == &kt_type_char_range;
    kt_boolean unsigned_bounds = kt_range_unsigned(self->header.type);
    KByteArray *buffer = kt_bytes_new(48);
    char *out = kt_bytes_of(buffer);
    kt_int length = chars ? kt_render_char((kt_char)range->first, out)
                    : unsigned_bounds ? kt_render_ulong((uint64_t)range->first, out)
                                      : kt_render_long(range->first, out);
    out[length++] = '.';
    out[length++] = '.';
    length += chars ? kt_render_char((kt_char)range->last, out + length)
              : unsigned_bounds ? kt_render_ulong((uint64_t)range->last, out + length)
                                : kt_render_long(range->last, out + length);
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
    uint32_t stride = destination->header.type->element_size;
    if (at < 0 || length < 0 || at + length > ((const KArray *)destination)->length) {
        kt_index_out_of_bounds(at + length, ((const KArray *)destination)->length);
    }
    memcpy((char *)(KArray *)destination + sizeof(KArray) + (size_t)at * stride,
           (const char *)(const KArray *)source + sizeof(KArray), (size_t)length * stride);
    return at + length;
}

/* ---- lists --------------------------------------------------------------------------------- */

/* `listOf(...)` as a VALUE. The elements are an ordinary `Array<T>` the list holds, which is what
   Kotlin's own `listOf(vararg)` does with the array a vararg call already built -- so the elements
   are traced by the collector through the array it already knows how to trace, and the list itself
   has exactly one reference field.

   The list is IMMUTABLE, which is what makes sharing the vararg array sound: nothing a program can
   write through reaches it. `MutableList` is not this type and is not realized here. */
typedef struct KList {
    KObjectHeader header;
    KRef elements;
} KList;

static const uint32_t kt_list_offsets[] = {offsetof(KList, elements)};

static kt_boolean kt_list_equals(KRef self, KRef other);
static kt_int kt_list_hash_code(KRef self);
static KRef kt_list_to_string(KRef self);

static const kt_fn kt_list_vtable[] = {(kt_fn)kt_list_equals, (kt_fn)kt_list_hash_code,
                                       (kt_fn)kt_list_to_string};

const KType kt_type_list = {"kotlin.collections.List",
                            sizeof("kotlin.collections.List") - 1,
                            sizeof(KList),
                            1,
                            0,
                            kt_list_offsets,
                            &kt_type_any,
                            kt_list_vtable,
                            3,
                            0};

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
       `remove` the cursor and the size can agree again and nothing else would look wrong. */
    kt_int modifications;
} KMutableList;

static const uint32_t kt_mutable_list_offsets[] = {offsetof(KMutableList, elements)};

static kt_boolean kt_list_equals(KRef self, KRef other);
static kt_int kt_list_hash_code(KRef self);
static KRef kt_list_to_string(KRef self);

/* Its `equals`/`hashCode`/`toString` are the list ones: Kotlin compares any two lists by their
   elements in order, and a `List` is equal to a `MutableList` holding the same things. */
static const kt_fn kt_mutable_list_vtable[] = {(kt_fn)kt_list_equals, (kt_fn)kt_list_hash_code,
                                               (kt_fn)kt_list_to_string};

const KType kt_type_mutable_list = {"kotlin.collections.ArrayList",
                                    sizeof("kotlin.collections.ArrayList") - 1,
                                    sizeof(KMutableList),
                                    1,
                                    0,
                                    kt_mutable_list_offsets,
                                    &kt_type_any,
                                    kt_mutable_list_vtable,
                                    3,
                                    0};

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
    kt_int modifications;
} KListIterator;

static const uint32_t kt_list_iterator_offsets[] = {offsetof(KListIterator, list)};

/* An iterator answers `kotlin.Any`'s three members by identity, as Kotlin's own iterators do. */
const KType kt_type_list_iterator = {"kotlin.collections.Iterator",
                                     sizeof("kotlin.collections.Iterator") - 1,
                                     sizeof(KListIterator),
                                     1,
                                     0,
                                     kt_list_iterator_offsets,
                                     &kt_type_any,
                                     kt_any_vtable,
                                     3,
                                     0};

static KRef *kt_elements_of(KRef array) { return (KRef *)((KArray *)array + 1); }

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
        kt_index_out_of_bounds(index, size);
    }
    return kt_elements_of(elements)[index];
}

/* `first()` and `last()`. Kotlin raises `NoSuchElementException` on an empty list, with its own
   wording, rather than answering NULL — a list of a nullable element type has a perfectly good
   NULL first element and the two must stay distinguishable. */
KRef kt_list_first(KRef list) {
    if (kt_list_size(list) == 0) {
        kt_throw(kt_throwable_new(&kt_type_no_such_element_exception,
                                  kt_string_utf8("List is empty.", 14)));
    }
    return kt_elements_of(((const KList *)list)->elements)[0];
}

KRef kt_list_last(KRef list) {
    kt_int size = kt_list_size(list);
    if (size == 0) {
        kt_throw(kt_throwable_new(&kt_type_no_such_element_exception,
                                  kt_string_utf8("List is empty.", 14)));
    }
    return kt_elements_of(((const KList *)list)->elements)[size - 1];
}

kt_int kt_list_index_of(KRef list, KRef value) {
    KRef elements = ((const KList *)list)->elements;
    kt_int length = kt_list_size(list);
    for (kt_int i = 0; i < length; i++) {
        if (kt_equals(kt_elements_of(elements)[i], value)) {
            return i;
        }
    }
    return -1;
}

kt_int kt_list_last_index_of(KRef list, KRef value) {
    KRef elements = ((const KList *)list)->elements;
    for (kt_int i = kt_list_size(list) - 1; i >= 0; i--) {
        if (kt_equals(kt_elements_of(elements)[i], value)) {
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

/* `ArrayList(initialCapacity)`. The capacity is a hint and nothing observable depends on it, so an
   invalid one is not a failure here — Kotlin's own throws, which this target will do once a
   `catch` exists to see the difference. */
KRef kt_mutable_list_with_capacity(kt_int capacity) {
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
    kt_int grown = capacity == 0 ? 4 : capacity * 2;
    /* The allocation can collect, and `self` is a root in the caller's frame, so the OLD array
       stays reachable through it until the new one is stored. */
    KRef replacement = kt_array_new(&kt_type_array, grown);
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
   order, which is the one walk this runtime already knows how to do. */
void kt_mutable_list_add_all(KRef self, KRef elements) {
    KRef iterator = kt_iterable_iterator(elements);
    while (kt_iterator_has_next(iterator)) {
        kt_mutable_list_add(self, kt_iterator_next(iterator));
    }
}

KRef kt_mutable_list_set(KRef self, kt_int index, KRef value) {
    KMutableList *list = (KMutableList *)self;
    if (index < 0 || index >= list->size) {
        kt_index_out_of_bounds(index, list->size);
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
    KListIterator *iterator =
        (KListIterator *)kt_gc_allocate(&kt_type_list_iterator, sizeof(KListIterator));
    iterator->list = list;
    iterator->at = 0;
    iterator->modifications = kt_is_mutable_list(list) ? ((const KMutableList *)list)->modifications
                                                       : 0;
    return (KRef)iterator;
}

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
} KWalk;

static const uint32_t kt_walk_offsets[] = {offsetof(KWalk, over)};

const KType kt_type_array_iterator = {"kotlin.collections.Iterator",
                                      sizeof("kotlin.collections.Iterator") - 1,
                                      sizeof(KWalk),
                                      1,
                                      0,
                                      kt_walk_offsets,
                                      &kt_type_any,
                                      kt_any_vtable,
                                      3,
                                      0};

const KType kt_type_chars_iterator = {"kotlin.collections.CharIterator",
                                      sizeof("kotlin.collections.CharIterator") - 1,
                                      sizeof(KWalk),
                                      1,
                                      0,
                                      kt_walk_offsets,
                                      &kt_type_any,
                                      kt_any_vtable,
                                      3,
                                      0};

static kt_boolean kt_walk_is(KRef iterator) {
    return iterator != NULL
           && (iterator->header.type == &kt_type_array_iterator
               || iterator->header.type == &kt_type_chars_iterator);
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
    return (KRef)walk;
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
        KT_FAIL("krusty: a list of a null array\n");
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

KRef kt_array_reversed(KRef array) { return kt_array_snapshot(array, 1); }

/* `xs.reversedArray()`: a new ARRAY of the same element type, backwards.

   Unlike `reversed()`, which answers a LIST of boxes, this keeps the elements where they were — in
   an array wearing the receiver's own descriptor, so a primitive array stays primitive. The
   elements are copied as BYTES: the descriptor's stride is what says how wide one is, and copying
   by width is the one answer that serves a reference array and a `DoubleArray` alike. */
KRef kt_array_reversed_array(KRef array) {
    if (array == NULL) {
        KT_FAIL("krusty: member access on a null receiver\n");
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
        if (!kt_equals(kt_array_element(left, index), kt_array_element(right, index))) {
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
    }
    kt_string_builder_append(builder, kt_string_utf8("]", 1));
    return kt_to_string(builder);
}

static kt_boolean kt_walk_has_next(KRef iterator) {
    const KWalk *walk = (const KWalk *)iterator;
    if (iterator->header.type == &kt_type_chars_iterator) {
        return walk->at < kt_string_length(walk->over);
    }
    return walk->at < kt_length_of(walk->over);
}

/* The element as the 64 bits the narrow iterator protocol carries. A REFERENCE array's element is
   not a number and must never arrive here: an `Array<T>`'s iterator has the static type
   `Iterator<T>`, which routes to the general dispatch instead, so reaching this with one means the
   routing above went wrong rather than that a pointer should be returned as an integer. */
static kt_long kt_walk_next_long(KRef iterator) {
    KWalk *walk = (KWalk *)iterator;
    if (!kt_walk_has_next(iterator)) {
        KT_FAIL("krusty: no more elements in this iterator\n");
    }
    kt_int at = walk->at;
    walk->at = at + 1;
    if (iterator->header.type == &kt_type_chars_iterator) {
        return kt_string_get(walk->over, at);
    }
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

const KType kt_type_indexed_value = {"kotlin.collections.IndexedValue",
                                     sizeof("kotlin.collections.IndexedValue") - 1,
                                     sizeof(KIndexedValue),
                                     1,
                                     0,
                                     kt_indexed_value_offsets,
                                     &kt_type_any,
                                     kt_indexed_value_vtable,
                                     3,
                                     0};

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
        KT_FAIL("krusty: member access on a null receiver\n");
    }
    return ((const KIndexedValue *)self)->index;
}

KRef kt_indexed_value_value(KRef self) {
    if (self == NULL) {
        KT_FAIL("krusty: member access on a null receiver\n");
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
    return mine->index == theirs->index && kt_equals(mine->value, theirs->value);
}

static kt_int kt_indexed_value_hash_code(KRef self) {
    const KIndexedValue *indexed = (const KIndexedValue *)self;
    return indexed->index * 31 + kt_hash_code(indexed->value);
}

static KRef kt_indexed_value_to_string(KRef self) {
    const KIndexedValue *indexed = (const KIndexedValue *)self;
    KRef text = kt_string_utf8("IndexedValue(index=", 19);
    text = kt_string_plus(text, kt_to_string(kt_box_int(indexed->index)));
    text = kt_string_plus(text, kt_string_utf8(", value=", 8));
    text = kt_string_plus(text, kt_to_string(indexed->value));
    return kt_string_plus(text, kt_string_utf8(")", 1));
}

/* What `withIndex()` answers: the source iterable, kept until somebody asks it for an iterator.
   Lazy, because Kotlin's is and because the loop that consumes it may stop early. */
typedef struct KWithIndex {
    KObjectHeader header;
    KRef source;
} KWithIndex;

static const uint32_t kt_with_index_offsets[] = {offsetof(KWithIndex, source)};

const KType kt_type_with_index = {"kotlin.collections.IndexingIterable",
                                  sizeof("kotlin.collections.IndexingIterable") - 1,
                                  sizeof(KWithIndex),
                                  1,
                                  0,
                                  kt_with_index_offsets,
                                  &kt_type_any,
                                  kt_any_vtable,
                                  3,
                                  0};

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

const KType kt_type_sequence = {"kotlin.sequences.Sequence",
                                sizeof("kotlin.sequences.Sequence") - 1,
                                sizeof(KSequence),
                                1,
                                0,
                                kt_sequence_offsets,
                                &kt_type_any,
                                kt_any_vtable,
                                3,
                                0};

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
    kt_int at;
} KIndexingIterator;

static const uint32_t kt_indexing_iterator_offsets[] = {offsetof(KIndexingIterator, source)};

const KType kt_type_indexing_iterator = {"kotlin.collections.IndexingIterator",
                                         sizeof("kotlin.collections.IndexingIterator") - 1,
                                         sizeof(KIndexingIterator),
                                         1,
                                         0,
                                         kt_indexing_iterator_offsets,
                                         &kt_type_any,
                                         kt_any_vtable,
                                         3,
                                         0};

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
        return kt_walk_of(&kt_type_array_iterator, iterable);
    }
    /* Either text shape: the chars iterator reads its element through `kt_string_get` and its
       bound through `kt_string_length`, and both answer for a string and for a builder. */
    if (iterable != NULL
        && (iterable->header.type == &kt_type_string || kt_is_string_builder(iterable))) {
        return kt_walk_of(&kt_type_chars_iterator, iterable);
    }
    /* A SET is iterated as the list of its elements: that list IS the set's order, which is the
       insertion order a `LinkedHashSet` promises. */
    if (kt_is_set(iterable)) {
        return kt_list_iterator(kt_map_keys_list(iterable));
    }
    /* A MAP is walked as its entries, which is what Kotlin's `Map.iterator()` extension answers
       and what a `for ((k, v) in m)` destructures. */
    if (kt_is_map(iterable)) {
        return kt_iterable_iterator(kt_map_entries(iterable));
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
        counting->at = 0;
        return (KRef)counting;
    }
    return kt_range_iterator(iterable);
}

kt_boolean kt_iterator_has_next(KRef iterator) {
    if (iterator != NULL && iterator->header.type == &kt_type_list_iterator) {
        return kt_list_iterator_has_next(iterator);
    }
    if (iterator != NULL && iterator->header.type == &kt_type_array_iterator) {
        const KWalk *walk = (const KWalk *)iterator;
        return walk->at < kt_length_of(walk->over);
    }
    if (iterator != NULL && iterator->header.type == &kt_type_chars_iterator) {
        const KWalk *walk = (const KWalk *)iterator;
        return walk->at < kt_string_length(walk->over);
    }
    if (iterator != NULL && iterator->header.type == &kt_type_indexing_iterator) {
        return kt_iterator_has_next(((const KIndexingIterator *)iterator)->source);
    }
    return kt_range_iterator_has_next(iterator);
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
   allocation, sized once, rather than a buffer that grows. */
static kt_int kt_iterable_size(KRef iterable) {
    if (iterable == NULL) {
        KT_FAIL("krusty: member access on a null receiver\n");
    }
    if (iterable->header.type == &kt_type_list || kt_is_mutable_list(iterable)) {
        return kt_list_size(iterable);
    }
    if (kt_is_set(iterable) || kt_is_map(iterable)) {
        return kt_map_size(iterable);
    }
    if (kt_is_array(iterable->header.type)) {
        return kt_length_of(iterable);
    }
    if (iterable->header.type == &kt_type_string || kt_is_string_builder(iterable)) {
        return kt_string_length(iterable);
    }
    if (iterable->header.type != &kt_type_int_range &&
        iterable->header.type != &kt_type_uint_range &&
        iterable->header.type != &kt_type_ulong_range &&
        iterable->header.type != &kt_type_long_range &&
        iterable->header.type != &kt_type_char_range) {
        KT_FAIL("krusty: this iterable cannot be counted\n");
    }
    /* How many elements the WALK yields, which is not `last - first + 1` unless the step is 1 and
       the walk ascends. A progression wears a range's descriptor here -- one struct serves both --
       so a descending `3 downTo 1` reached the ascending test with `first` above `last` and was
       counted as empty, and `1..9 step 3` would have been counted 7 where the walk yields 3.

       `kt_range_empty` already answers emptiness for either direction and at the bounds' own
       signedness, so the count below never divides for a walk that yields nothing.

       The span is taken on the RING, as two's-complement subtraction, for the reason
       `kt_progression_last` states about forming a distance: `Long.MIN_VALUE..Long.MAX_VALUE`
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
    uint64_t magnitude = (uint64_t)(step > 0 ? step : -step);
    uint64_t count = span / magnitude + 1u;
    if (count > (uint64_t)INT32_MAX) {
        KT_FAIL("krusty: a range too long to collect\n");
    }
    return (kt_int)count;
}

/* `xs.map { … }`: one new list, the transform applied to each element in order.

   The result list is built BEFORE the loop so that it, and through it the array, is a root across
   every call the loop makes — each of which may collect, and each of which may allocate whatever
   the transform returns. Elements already written are traced through the array like any other
   reference, so there is nothing to defer and no barrier to write. */
KRef kt_iterable_map(KRef iterable, KRef transform) {
    kt_int size = kt_iterable_size(iterable);
    KRef elements = kt_array_new(&kt_type_array, size);
    KRef result = kt_list_of(elements);
    KRef iterator = kt_iterable_iterator(iterable);
    for (kt_int i = 0; i < size; i++) {
        kt_elements_of(elements)[i] = kt_invoke_one(transform, kt_iterator_next(iterator));
    }
    return result;
}

KRef kt_iterable_join_to_string(KRef iterable) {
    KRef separator = kt_string_utf8(", ", 2);
    KRef joined = kt_string_utf8("", 0);
    KRef iterator = kt_iterable_iterator(iterable);
    kt_boolean first = 1;
    while (kt_iterator_has_next(iterator)) {
        if (!first) {
            joined = kt_string_plus(joined, separator);
        }
        first = 0;
        /* `kt_to_string` and not the element itself: `joinToString` renders each element the way
           `"$element"` would, through whatever `toString` the element's own type answers with. */
        joined = kt_string_plus(joined, kt_to_string(kt_iterator_next(iterator)));
    }
    return joined;
}

/* `xs.forEach { … }`: the same walk with nothing kept, and so nothing to size. */
void kt_iterable_for_each(KRef iterable, KRef action) {
    KRef iterator = kt_iterable_iterator(iterable);
    while (kt_iterator_has_next(iterator)) {
        kt_invoke_one(action, kt_iterator_next(iterator));
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

/* Whether a predicate answered true for an element. The answer arrives BOXED, because a function
   value's `invoke` hands back a reference whatever its declared return type is. */
static kt_boolean kt_holds(KRef predicate, KRef element) {
    return kt_unbox_boolean(kt_invoke_one(predicate, element));
}

/* `xs.any { … }`, `xs.all { … }` and `xs.none { … }` — one walk, three readings of it. Each stops
   at the first element that settles the question, which is Kotlin's own promise and is observable
   through a predicate with a side effect. */
kt_boolean kt_iterable_any(KRef iterable, KRef predicate) {
    KRef iterator = kt_iterable_iterator(iterable);
    while (kt_iterator_has_next(iterator)) {
        if (kt_holds(predicate, kt_iterator_next(iterator))) {
            return 1;
        }
    }
    return 0;
}

kt_boolean kt_iterable_all(KRef iterable, KRef predicate) {
    KRef iterator = kt_iterable_iterator(iterable);
    while (kt_iterator_has_next(iterator)) {
        if (!kt_holds(predicate, kt_iterator_next(iterator))) {
            return 0;
        }
    }
    return 1;
}

kt_boolean kt_iterable_none(KRef iterable, KRef predicate) {
    return !kt_iterable_any(iterable, predicate);
}

/* `xs.any()` and `xs.none()` with no predicate: whether the walk yields anything at all. */
kt_boolean kt_iterable_is_not_empty(KRef iterable) {
    return kt_iterator_has_next(kt_iterable_iterator(iterable));
}

kt_boolean kt_iterable_is_empty(KRef iterable) { return !kt_iterable_is_not_empty(iterable); }

/* `xs.count()` walks rather than reading a size: `count` is declared over `Iterable`, and the
   walk is the only thing every iterable has. */
kt_int kt_iterable_count(KRef iterable) {
    KRef iterator = kt_iterable_iterator(iterable);
    kt_int counted = 0;
    while (kt_iterator_has_next(iterator)) {
        (void)kt_iterator_next(iterator);
        counted++;
    }
    return counted;
}

kt_int kt_iterable_count_matching(KRef iterable, KRef predicate) {
    KRef iterator = kt_iterable_iterator(iterable);
    kt_int counted = 0;
    while (kt_iterator_has_next(iterator)) {
        if (kt_holds(predicate, kt_iterator_next(iterator))) {
            counted++;
        }
    }
    return counted;
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
    while (kt_iterator_has_next(iterator)) {
        KRef element = kt_iterator_next(iterator);
        if (kt_holds(predicate, element) == keep) {
            kt_mutable_list_add(growing, element);
        }
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
    while (kt_iterator_has_next(iterator)) {
        KRef element = kt_iterator_next(iterator);
        if (kt_holds(predicate, element)) {
            return element;
        }
    }
    return NULL;
}

KRef kt_iterable_first_matching(KRef iterable, KRef predicate) {
    KRef iterator = kt_iterable_iterator(iterable);
    while (kt_iterator_has_next(iterator)) {
        KRef element = kt_iterator_next(iterator);
        if (kt_holds(predicate, element)) {
            return element;
        }
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
    while (kt_iterator_has_next(iterator)) {
        KRef element = kt_iterator_next(iterator);
        if (kt_holds(predicate, element)) {
            found = element;
            any = 1;
        }
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
    while (kt_iterator_has_next(iterator)) {
        accumulator = kt_invoke_two(operation, accumulator, kt_iterator_next(iterator));
    }
    return accumulator;
}

/* `xs.forEachIndexed { i, e -> … }`. The index is BOXED on the way in, because a function value
   takes references; the lambda's own prologue unboxes it. */
void kt_iterable_for_each_indexed(KRef iterable, KRef action) {
    KRef iterator = kt_iterable_iterator(iterable);
    kt_int index = 0;
    while (kt_iterator_has_next(iterator)) {
        KRef element = kt_iterator_next(iterator);
        (void)kt_invoke_two(action, kt_box_int(index), element);
        index++;
    }
}

/* `xs.toList()` and `xs.reversed()` over an ITERABLE: a snapshot of its elements, in order or
   backwards. The array version of both is `kt_array_to_list`/`kt_array_reversed`; this one walks,
   which is what a range and a lazy `withIndex()` need. */
static KRef kt_iterable_snapshot(KRef iterable, kt_boolean reversed) {
    KRef growing = kt_mutable_list_new();
    KRef iterator = kt_iterable_iterator(iterable);
    while (kt_iterator_has_next(iterator)) {
        kt_mutable_list_add(growing, kt_iterator_next(iterator));
    }
    return kt_frozen(growing, reversed);
}

KRef kt_iterable_to_list(KRef iterable) { return kt_iterable_snapshot(iterable, 0); }

KRef kt_iterable_reversed(KRef iterable) { return kt_iterable_snapshot(iterable, 1); }

/* `value in xs` and `xs.indexOf(value)` over an ITERABLE. Elements are compared with `equals`, as
   Kotlin's own are — the list form already does, and a range's is the same question asked of the
   numbers it yields. */
kt_int kt_iterable_index_of(KRef iterable, KRef value) {
    KRef iterator = kt_iterable_iterator(iterable);
    kt_int at = 0;
    while (kt_iterator_has_next(iterator)) {
        if (kt_equals(kt_iterator_next(iterator), value)) {
            return at;
        }
        at++;
    }
    return -1;
}

kt_boolean kt_iterable_contains(KRef iterable, KRef value) {
    return kt_iterable_index_of(iterable, value) >= 0;
}

/* `xs + x` and `xs + ys`: a NEW read-only list, never a change to the receiver — that is what
   separates `plus` from `plusAssign`, and Kotlin's contract is that a `List` cannot be changed at
   all. Which of the two a call means is the CALLER's answer, read from the physical parameter the
   same way `plusAssign` reads it: after substitution an element of type `List<T>` and a collection
   of them look alike, and only the declaration tells them apart. */
static KRef kt_iterable_walked_into(KRef iterable, KRef growing) {
    KRef iterator = kt_iterable_iterator(iterable);
    while (kt_iterator_has_next(iterator)) {
        kt_mutable_list_add(growing, kt_iterator_next(iterator));
    }
    return growing;
}

KRef kt_iterable_plus_element(KRef iterable, KRef element) {
    KRef growing = kt_iterable_walked_into(iterable, kt_mutable_list_new());
    kt_mutable_list_add(growing, element);
    return kt_frozen(growing, 0);
}

KRef kt_iterable_plus_all(KRef iterable, KRef tail) {
    KRef growing = kt_iterable_walked_into(iterable, kt_mutable_list_new());
    return kt_frozen(kt_iterable_walked_into(tail, growing), 0);
}

/* `xs.sumOf { … }`. Kotlin declares one per width the selector may answer, and the answer's TYPE
   is the selector's — so which of these a call reaches is decided where the declaration is in
   sight, and each unboxes what `invoke` hands back at the width its own name says. Summing at one
   width and narrowing afterwards would not do: `Int` addition wraps and `Long` addition does not,
   and a program that sums to an overflow is entitled to Kotlin's answer. */
kt_int kt_iterable_sum_of_int(KRef iterable, KRef selector) {
    KRef iterator = kt_iterable_iterator(iterable);
    kt_int total = 0;
    while (kt_iterator_has_next(iterator)) {
        total = (kt_int)((uint32_t)total
                         + (uint32_t)kt_unbox_int(kt_invoke_one(selector,
                                                                kt_iterator_next(iterator))));
    }
    return total;
}

kt_long kt_iterable_sum_of_long(KRef iterable, KRef selector) {
    KRef iterator = kt_iterable_iterator(iterable);
    kt_long total = 0;
    while (kt_iterator_has_next(iterator)) {
        total = (kt_long)((uint64_t)total
                          + (uint64_t)kt_unbox_long(kt_invoke_one(selector,
                                                                  kt_iterator_next(iterator))));
    }
    return total;
}

kt_double kt_iterable_sum_of_double(KRef iterable, KRef selector) {
    KRef iterator = kt_iterable_iterator(iterable);
    kt_double total = 0.0;
    while (kt_iterator_has_next(iterator)) {
        total += kt_unbox_double(kt_invoke_one(selector, kt_iterator_next(iterator)));
    }
    return total;
}

KRef kt_iterator_next(KRef iterator) {
    if (iterator == NULL) {
        KT_FAIL("krusty: member access on a null receiver\n");
    }
    if (iterator->header.type == &kt_type_list_iterator) {
        return kt_list_iterator_next(iterator);
    }
    if (iterator->header.type == &kt_type_array_iterator) {
        KWalk *walk = (KWalk *)iterator;
        kt_int at = walk->at;
        walk->at = at + 1;
        return kt_array_element(walk->over, at);
    }
    if (iterator->header.type == &kt_type_chars_iterator) {
        KWalk *walk = (KWalk *)iterator;
        kt_int at = walk->at;
        walk->at = at + 1;
        return kt_box_char(kt_string_get(walk->over, at));
    }
    if (iterator->header.type == &kt_type_indexing_iterator) {
        KIndexingIterator *counting = (KIndexingIterator *)iterator;
        /* The element is fetched BEFORE the index is bumped, and it stays in a local across the
           allocation below so the collector sees it as a root. */
        KRef element = kt_iterator_next(counting->source);
        kt_int at = counting->at;
        counting->at = at + 1;
        return kt_indexed_value(at, element);
    }
    kt_long value = kt_range_iterator_next(iterator);
    if (iterator->header.type == &kt_type_long_iterator) {
        return kt_box_long(value);
    }
    if (iterator->header.type == &kt_type_char_iterator) {
        return kt_box_char((kt_char)value);
    }
    if (iterator->header.type == &kt_type_uint_iterator) {
        return kt_box_uint((kt_int)value);
    }
    if (iterator->header.type == &kt_type_ulong_iterator) {
        return kt_box_ulong(value);
    }
    return kt_box_int((kt_int)value);
}

/* Kotlin's `List.equals`: same size and elementwise equal, and only against another list. A list
   never equals a set with the same members, which the type comparison is. */
static kt_boolean kt_list_equals(KRef self, KRef other) {
    if (self == other) {
        return true;
    }
    /* Either shape counts as a list: Kotlin compares two lists by their elements in order, so a
       `List` and a `MutableList` holding the same things are equal. */
    if (other == NULL
        || (other->header.type != &kt_type_list && !kt_is_mutable_list(other))) {
        return false;
    }
    kt_int size = kt_list_size(self);
    if (size != kt_list_size(other)) {
        return false;
    }
    KRef left = ((const KList *)self)->elements;
    KRef right = ((const KList *)other)->elements;
    for (kt_int i = 0; i < size; i++) {
        if (!kt_equals(kt_elements_of(left)[i], kt_elements_of(right)[i])) {
            return false;
        }
    }
    return true;
}

/* Kotlin's own: 1 folded with `31 * h + e.hashCode()`, a null element contributing 0. */
static kt_int kt_list_hash_code(KRef self) {
    KRef elements = ((const KList *)self)->elements;
    kt_int length = kt_list_size(self);
    uint32_t hash = 1;
    for (kt_int i = 0; i < length; i++) {
        hash = 31u * hash + (uint32_t)kt_hash_code(kt_elements_of(elements)[i]);
    }
    return (kt_int)hash;
}

/* `[a, b, c]`, each element through its own `toString` — which is what makes this a loop over
   `kt_string_plus` rather than a render into one buffer: an element's rendering may itself
   allocate, and the joined text has to stay reachable across that. */
static KRef kt_list_to_string(KRef self) {
    KRef elements = ((const KList *)self)->elements;
    kt_int length = kt_list_size(self);
    KRef text = kt_string_utf8("[", 1);
    for (kt_int i = 0; i < length; i++) {
        if (i > 0) {
            text = kt_string_plus(text, kt_string_utf8(", ", 2));
        }
        text = kt_string_plus(text, kt_to_string(kt_elements_of(elements)[i]));
    }
    return kt_string_plus(text, kt_string_utf8("]", 1));
}


/* ---- maps and sets --------------------------------------------------------------------------

   A map is two growable lists side by side: its keys in insertion order, and the values beside
   them at the same positions. A SET is the same object with no values, which is what Kotlin's own
   `LinkedHashSet` is — a map whose values nothing reads.

   Lookup is LINEAR, by `equals` over the keys. Kotlin's is by hash, and the difference is speed
   and nothing else: a hash map answers the same question, and the maps a program writes in a box
   test hold a handful of entries. What a hash map would NOT give is the order, and order is the
   observable part: `mapOf` answers a `LinkedHashMap`, whose iteration, `toString` and `keys` are
   in insertion order. Keeping the keys in a list rather than in buckets is what that promise asks
   for. The unordered spellings — `hashMapOf`, `HashSet()` — answer this object too, because their
   order is unspecified and insertion order is one of the orders left unspecified.

   Both growable lists are reference fields the collector traces, and every element inside them is
   traced through the array each list already holds. */
typedef struct KMap {
    KObjectHeader header;
    KRef keys;
    /* The values, at the same positions as the keys — or NULL for a set, which has none. */
    KRef values;
} KMap;

static const uint32_t kt_map_offsets[] = {offsetof(KMap, keys), offsetof(KMap, values)};

static kt_boolean kt_map_equals(KRef self, KRef other);
static kt_int kt_map_hash_code(KRef self);
static KRef kt_map_to_string(KRef self);
static kt_boolean kt_set_equals(KRef self, KRef other);
static kt_int kt_set_hash_code(KRef self);
static KRef kt_set_to_string(KRef self);

static const kt_fn kt_map_vtable[] = {(kt_fn)kt_map_equals, (kt_fn)kt_map_hash_code,
                                      (kt_fn)kt_map_to_string};
static const kt_fn kt_set_vtable[] = {(kt_fn)kt_set_equals, (kt_fn)kt_set_hash_code,
                                      (kt_fn)kt_set_to_string};

#define KT_MAP_TYPE(identifier, kotlin_name, table)                                                \
    const KType identifier = {kotlin_name, sizeof(kotlin_name) - 1,                                \
                              sizeof(KMap), 2,                                                     \
                              0,           kt_map_offsets,                                         \
                              &kt_type_any, table,                                                 \
                              3,           0};

KT_MAP_TYPE(kt_type_map, "kotlin.collections.LinkedHashMap", kt_map_vtable)
KT_MAP_TYPE(kt_type_set, "kotlin.collections.LinkedHashSet", kt_set_vtable)

/* One entry of a map, which `entries` hands out and a destructuring reads through
   `component1`/`component2`. It is a VIEW of nothing: the pair is copied out, so writing to the
   map afterwards leaves an entry already taken alone. Kotlin's own entry is a view and setting
   through it writes back, which `MutableMap.MutableEntry.setValue` is for; nothing here answers
   that member, so the copy is not observable. */
typedef struct KMapEntry {
    KObjectHeader header;
    KRef key;
    KRef value;
} KMapEntry;

static const uint32_t kt_map_entry_offsets[] = {offsetof(KMapEntry, key),
                                                offsetof(KMapEntry, value)};

static kt_boolean kt_map_entry_equals(KRef self, KRef other);
static kt_int kt_map_entry_hash_code(KRef self);
static KRef kt_map_entry_to_string(KRef self);

static const kt_fn kt_map_entry_vtable[] = {(kt_fn)kt_map_entry_equals,
                                            (kt_fn)kt_map_entry_hash_code,
                                            (kt_fn)kt_map_entry_to_string};

const KType kt_type_map_entry = {"kotlin.collections.Map.Entry",
                                 sizeof("kotlin.collections.Map.Entry") - 1,
                                 sizeof(KMapEntry),
                                 2,
                                 0,
                                 kt_map_entry_offsets,
                                 &kt_type_any,
                                 kt_map_entry_vtable,
                                 3,
                                 0};

kt_boolean kt_is_map(KRef value) { return value != NULL && value->header.type == &kt_type_map; }

kt_boolean kt_is_set(KRef value) { return value != NULL && value->header.type == &kt_type_set; }

/* The keys, which for a set ARE its elements — so one walk serves both and iterating a set is
   iterating this list. */
KRef kt_map_keys_list(KRef self) { return ((const KMap *)self)->keys; }

static KRef kt_map_shaped(const KType *type, kt_boolean valued) {
    KMap *map = (KMap *)kt_gc_allocate(type, sizeof(KMap));
    /* Both fields are stored before either allocation, so a collection triggered by one never
       traces an uninitialized field. */
    map->keys = NULL;
    map->values = NULL;
    map->keys = kt_mutable_list_new();
    if (valued) {
        map->values = kt_mutable_list_new();
    }
    return (KRef)map;
}

KRef kt_map_new(void) { return kt_map_shaped(&kt_type_map, 1); }

KRef kt_set_new(void) { return kt_map_shaped(&kt_type_set, 0); }

kt_int kt_map_size(KRef self) { return kt_list_size(((const KMap *)self)->keys); }

kt_boolean kt_map_is_empty(KRef self) { return kt_map_size(self) == 0; }

/* Where a key sits, or -1. By `equals`, as Kotlin's own lookup is: two strings with the same text
   are one key, and so are two boxes holding the same number. */
static kt_int kt_map_index_of(KRef self, KRef key) {
    return kt_list_index_of(((const KMap *)self)->keys, key);
}

kt_boolean kt_map_contains_key(KRef self, KRef key) { return kt_map_index_of(self, key) >= 0; }

kt_boolean kt_map_contains_value(KRef self, KRef value) {
    const KMap *map = (const KMap *)self;
    return map->values != NULL && kt_list_contains(map->values, value);
}

/* `m[k]`. Kotlin answers NULL for an absent key, which is why `Map.get` is declared nullable and
   why a map whose values are nullable cannot tell the two apart either. */
KRef kt_map_get(KRef self, KRef key) {
    kt_int at = kt_map_index_of(self, key);
    if (at < 0) {
        return NULL;
    }
    const KMap *map = (const KMap *)self;
    return map->values == NULL ? kt_list_get(map->keys, at) : kt_list_get(map->values, at);
}

KRef kt_map_get_or_default(KRef self, KRef key, KRef fallback) {
    kt_int at = kt_map_index_of(self, key);
    return at < 0 ? fallback : kt_list_get(((const KMap *)self)->values, at);
}

/* `m.put(k, v)`, answering the value that was there. An existing key keeps its POSITION, which is
   what a `LinkedHashMap` promises: re-putting a key does not move it to the end. */
KRef kt_map_put(KRef self, KRef key, KRef value) {
    KMap *map = (KMap *)self;
    kt_int at = kt_map_index_of(self, key);
    if (at >= 0) {
        return kt_mutable_list_set(map->values, at, value);
    }
    kt_mutable_list_add(map->keys, key);
    kt_mutable_list_add(map->values, value);
    return NULL;
}

/* `m[k] = v`, which answers `Unit` rather than the previous value — so it is its own entry point
   rather than a result the caller has to remember to drop. */
void kt_map_set(KRef self, KRef key, KRef value) { (void)kt_map_put(self, key, value); }

KRef kt_map_remove(KRef self, KRef key) {
    KMap *map = (KMap *)self;
    kt_int at = kt_map_index_of(self, key);
    if (at < 0) {
        return NULL;
    }
    (void)kt_mutable_list_remove_at(map->keys, at);
    return kt_mutable_list_remove_at(map->values, at);
}

void kt_map_clear(KRef self) {
    KMap *map = (KMap *)self;
    kt_mutable_list_clear(map->keys);
    if (map->values != NULL) {
        kt_mutable_list_clear(map->values);
    }
}

/* `s.add(x)` / `x in s` / `s.remove(x)`: a set keeps each element once, so adding one it already
   holds changes nothing and says so. */
kt_boolean kt_set_contains(KRef self, KRef value) { return kt_map_contains_key(self, value); }

kt_boolean kt_set_add(KRef self, KRef value) {
    if (kt_map_contains_key(self, value)) {
        return false;
    }
    kt_mutable_list_add(((KMap *)self)->keys, value);
    return true;
}

kt_boolean kt_set_remove(KRef self, KRef value) {
    kt_int at = kt_map_index_of(self, value);
    if (at < 0) {
        return false;
    }
    (void)kt_mutable_list_remove_at(((KMap *)self)->keys, at);
    return true;
}

/* `mapOf(a to b, …)` and `setOf(a, …)`, from the array a vararg call already packed. The array
   belongs to the CALLER, so its contents are copied in rather than shared: a map can be written
   through, and writing to one must not reach back into the caller's array. */
KRef kt_map_of(KRef pairs) {
    KRef map = kt_map_new();
    kt_int length = kt_length_of(pairs);
    for (kt_int at = 0; at < length; at++) {
        KRef pair = kt_elements_of(pairs)[at];
        (void)kt_map_put(map, kt_pair_first(pair), kt_pair_second(pair));
    }
    return map;
}

/* `mapOf(a to b)`: the ONE-pair form Kotlin declares beside the vararg one. */
KRef kt_map_of_pair(KRef pair) {
    KRef map = kt_map_new();
    (void)kt_map_put(map, kt_pair_first(pair), kt_pair_second(pair));
    return map;
}

KRef kt_set_of(KRef elements) {
    KRef set = kt_set_new();
    kt_int length = kt_length_of(elements);
    for (kt_int at = 0; at < length; at++) {
        (void)kt_set_add(set, kt_elements_of(elements)[at]);
    }
    return set;
}

/* `m.keys`, `m.values` and `m.entries`. Kotlin's are VIEWS onto the map; these are snapshots, and
   the difference shows only where a program keeps one across a write to the map. Answering a
   snapshot is the same trade `toList()` on an array makes, and it is what lets each of them be an
   object this runtime already has. */
KRef kt_map_keys(KRef self) {
    KRef keys = kt_set_new();
    KRef source = ((const KMap *)self)->keys;
    kt_int size = kt_list_size(source);
    for (kt_int at = 0; at < size; at++) {
        (void)kt_set_add(keys, kt_list_get(source, at));
    }
    return keys;
}

KRef kt_map_values(KRef self) {
    const KMap *map = (const KMap *)self;
    KRef source = map->values == NULL ? map->keys : map->values;
    kt_int size = kt_list_size(source);
    KRef elements = kt_array_new(&kt_type_array, size);
    KRef result = kt_list_of(elements);
    for (kt_int at = 0; at < size; at++) {
        kt_elements_of(elements)[at] = kt_list_get(source, at);
    }
    return result;
}

static KRef kt_map_entry_new(KRef key, KRef value) {
    KMapEntry *entry = (KMapEntry *)kt_gc_allocate(&kt_type_map_entry, sizeof(KMapEntry));
    entry->key = key;
    entry->value = value;
    return (KRef)entry;
}

KRef kt_map_entries(KRef self) {
    const KMap *map = (const KMap *)self;
    KRef entries = kt_set_new();
    kt_int size = kt_list_size(map->keys);
    for (kt_int at = 0; at < size; at++) {
        KRef key = kt_list_get(map->keys, at);
        KRef value = map->values == NULL ? key : kt_list_get(map->values, at);
        (void)kt_set_add(entries, kt_map_entry_new(key, value));
    }
    return entries;
}

KRef kt_map_entry_key(KRef entry) { return ((const KMapEntry *)entry)->key; }

KRef kt_map_entry_value(KRef entry) { return ((const KMapEntry *)entry)->value; }

/* Kotlin's own three for an entry: `k=v`, the two hashes xored, and equality by both halves. */
static kt_boolean kt_map_entry_equals(KRef self, KRef other) {
    if (other == NULL || other->header.type != &kt_type_map_entry) {
        return false;
    }
    const KMapEntry *a = (const KMapEntry *)self;
    const KMapEntry *b = (const KMapEntry *)other;
    return kt_equals(a->key, b->key) && kt_equals(a->value, b->value);
}

static kt_int kt_map_entry_hash_code(KRef self) {
    const KMapEntry *entry = (const KMapEntry *)self;
    kt_int key = entry->key == NULL ? 0 : kt_hash_code(entry->key);
    kt_int value = entry->value == NULL ? 0 : kt_hash_code(entry->value);
    return key ^ value;
}

static KRef kt_map_entry_to_string(KRef self) {
    const KMapEntry *entry = (const KMapEntry *)self;
    KRef text = kt_string_plus(kt_to_string(entry->key), kt_string_utf8("=", 1));
    return kt_string_plus(text, kt_to_string(entry->value));
}

/* Two maps are equal when they hold the same entries, whatever ORDER they hold them in — Kotlin's
   `Map.equals` says nothing about order and a `LinkedHashMap` equals a `HashMap` of the same
   entries. The hash is the sum of the entry hashes, which is order-independent for the same
   reason. */
static kt_boolean kt_map_equals(KRef self, KRef other) {
    if (other == NULL || other->header.type != &kt_type_map) {
        return false;
    }
    const KMap *a = (const KMap *)self;
    if (kt_map_size(self) != kt_map_size(other)) {
        return false;
    }
    kt_int size = kt_list_size(a->keys);
    for (kt_int at = 0; at < size; at++) {
        KRef key = kt_list_get(a->keys, at);
        if (!kt_map_contains_key(other, key)) {
            return false;
        }
        if (!kt_equals(kt_list_get(a->values, at), kt_map_get(other, key))) {
            return false;
        }
    }
    return true;
}

static kt_int kt_map_hash_code(KRef self) {
    const KMap *map = (const KMap *)self;
    kt_int size = kt_list_size(map->keys);
    uint32_t total = 0;
    for (kt_int at = 0; at < size; at++) {
        KRef key = kt_list_get(map->keys, at);
        KRef value = kt_list_get(map->values, at);
        uint32_t left = key == NULL ? 0u : (uint32_t)kt_hash_code(key);
        uint32_t right = value == NULL ? 0u : (uint32_t)kt_hash_code(value);
        total += left ^ right;
    }
    return (kt_int)total;
}

/* `{a=1, b=2}`, in insertion order, each half rendered through its own `toString`. */
static KRef kt_map_to_string(KRef self) {
    const KMap *map = (const KMap *)self;
    KRef text = kt_string_utf8("{", 1);
    kt_int size = kt_list_size(map->keys);
    for (kt_int at = 0; at < size; at++) {
        if (at != 0) {
            text = kt_string_plus(text, kt_string_utf8(", ", 2));
        }
        text = kt_string_plus(text, kt_to_string(kt_list_get(map->keys, at)));
        text = kt_string_plus(text, kt_string_utf8("=", 1));
        text = kt_string_plus(text, kt_to_string(kt_list_get(map->values, at)));
    }
    return kt_string_plus(text, kt_string_utf8("}", 1));
}

/* Two sets are equal when each holds what the other does, whatever order; the hash is the sum of
   the element hashes, which says the same thing. */
static kt_boolean kt_set_equals(KRef self, KRef other) {
    if (other == NULL || other->header.type != &kt_type_set) {
        return false;
    }
    if (kt_map_size(self) != kt_map_size(other)) {
        return false;
    }
    KRef keys = ((const KMap *)self)->keys;
    kt_int size = kt_list_size(keys);
    for (kt_int at = 0; at < size; at++) {
        if (!kt_set_contains(other, kt_list_get(keys, at))) {
            return false;
        }
    }
    return true;
}

static kt_int kt_set_hash_code(KRef self) {
    KRef keys = ((const KMap *)self)->keys;
    kt_int size = kt_list_size(keys);
    uint32_t total = 0;
    for (kt_int at = 0; at < size; at++) {
        KRef element = kt_list_get(keys, at);
        total += element == NULL ? 0u : (uint32_t)kt_hash_code(element);
    }
    return (kt_int)total;
}

/* `[a, b]` — a set renders as a collection does, which is what Kotlin's own answers. */
static KRef kt_set_to_string(KRef self) { return kt_list_to_string(((const KMap *)self)->keys); }

/* Static storage, not the heap: the collector never sees it as an object, and nothing needs it
   to. */
KRef kt_unit(void) {
    static KObject unit = {{&kt_type_unit}, {{NULL, NULL, 0}}};
    return &unit;
}

/* ---- classes ------------------------------------------------------------------------------- */

kt_boolean kt_any_equals(KRef self, KRef other) { return self == other; }

/* Derived from the address. The collector never moves an object (conservative roots forbid it;
   see krusty_gc.c), so an object's address is stable for its whole life and is a legitimate
   identity hash. The shifts fold the aligned low bits and the high bits into the 32 that count. */
kt_int kt_any_hash_code(KRef self) {
    uintptr_t address = (uintptr_t)self;
    return (kt_int)(uint32_t)((address >> 4) ^ (address >> 36));
}

/* `<qualified name>@<hex hashCode>`, Kotlin's default shape. */
KRef kt_any_to_string(KRef self) {
    const KType *type = self->header.type;
    uint32_t hash = (uint32_t)kt_hash_code(self);
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

static KRef kt_object_to_string(KRef value) {
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
static uint64_t kt_double_bits(kt_double value) {
    uint64_t bits;
    memcpy(&bits, &value, sizeof bits);
    if ((bits & 0x7FF0000000000000ULL) == 0x7FF0000000000000ULL &&
        (bits & 0x000FFFFFFFFFFFFFULL) != 0) {
        return 0x7FF8000000000000ULL;
    }
    return bits;
}

static uint32_t kt_float_bits(kt_float value) {
    uint32_t bits;
    memcpy(&bits, &value, sizeof bits);
    if ((bits & 0x7F800000u) == 0x7F800000u && (bits & 0x007FFFFFu) != 0) {
        return 0x7FC00000u;
    }
    return bits;
}


/* Built-in values compare by value, as Kotlin's `==` on boxed values does: two `Int?` holding 3
   are equal, and two strings with the same text are equal. */
static kt_boolean kt_builtin_equals(KRef self, KRef other) {
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
static kt_int kt_builtin_hash_code(KRef self) {
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
        KT_FAIL("krusty: member access on a null receiver\n");
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

KRef kt_cast_non_null(KRef object, const KType *type) {
    if (object == NULL) {
        // `null as String` is a NullPointerException NAMING the target type, not a
        // ClassCastException — `null` is not an instance of anything, so there is no class to
        // report as the source. kotlinc's exact wording, and its exact type.
        KRef message =
            kt_string_plus(kt_string_utf8("null cannot be cast to non-null type ", 37),
                           kt_string_utf8(type->name, type->name_length));
        kt_throw(kt_throwable_new(&kt_type_null_pointer_exception, message));
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

void kt_not_implemented_reason(KRef reason) {
    KT_THROW(kt_type_not_implemented_error,
             kt_string_plus(KT_MESSAGE("An operation is not implemented: "), kt_to_string(reason)));
}

/* `error(message)` takes an `Any`, and the exception carries its `toString`. */
void kt_illegal_state(KRef message) {
    KT_THROW(kt_type_illegal_state_exception, kt_to_string(message));
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

void kt_check(kt_boolean value) {
    if (!value) {
        KT_THROW(kt_type_illegal_state_exception, KT_MESSAGE("Check failed."));
    }
}

#undef KT_MESSAGE
#undef KT_THROW

void kt_abstract_method_called(void) { KT_FAIL("krusty: abstract method called\n"); }

void kt_null_receiver(void) { KT_FAIL("krusty: member access on a null receiver\n"); }

/* A callable whose Kotlin type is `Nothing` returned instead of diverging, and the path that reads
   its value has no value to read. The JVM throws `KotlinNothingValueException` here. This one is
   NOT raised as a `Throwable`, unlike the stdlib's own throwers: reaching it means a callee lied
   about its type, which is a defect in what was emitted rather than something a program is entitled
   to catch, so it stays the loud, uncatchable failure a failed cast is. */
void kt_nothing_value_returned(void) {
    KT_FAIL("krusty: a `Nothing`-typed callable returned a value\n");
}

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

/* Kotlin masks the shift count, so `1 shl 32` is `1`, not undefined. A right shift of a negative
   value is implementation-defined in C, so the arithmetic shift is spelled out instead of assumed. */
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

kt_byte kt_unbox_ubyte(KRef value) {
    if (value == NULL) {
        kt_throw(kt_throwable_new(&kt_type_null_pointer_exception, NULL));
    }
    return value->as.byte_value;
}

kt_short kt_unbox_ushort(KRef value) {
    if (value == NULL) {
        kt_throw(kt_throwable_new(&kt_type_null_pointer_exception, NULL));
    }
    return value->as.short_value;
}

kt_int kt_unbox_uint(KRef value) {
    if (value == NULL) {
        kt_throw(kt_throwable_new(&kt_type_null_pointer_exception, NULL));
    }
    return value->as.int_value;
}

kt_long kt_unbox_ulong(KRef value) {
    if (value == NULL) {
        kt_throw(kt_throwable_new(&kt_type_null_pointer_exception, NULL));
    }
    return value->as.long_value;
}

/* Render an unsigned 64-bit value into `buffer` (at least 20 bytes); returns the length written.
   Separate from `kt_render_long` because the signed one negates into unsigned space to reach the
   digits, which is exactly the step that must not happen here. */
static kt_int kt_render_ulong(uint64_t value, char *buffer) {
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

/* ---- kotlin.io ----------------------------------------------------------------------------- */

/* ---- exceptions ----------------------------------------------------------------------------

   A `Throwable` is an ordinary object with one reference field and a real `super` chain, which is
   the whole of what a `catch` needs: matching a clause is `kt_is_instance` against the clause's
   type, and that already walks this chain.

   `toString` is Kotlin's: the qualified name, and `: message` after it when there is one. A
   subclass declared in Kotlin source inherits this slot like any other. */

typedef struct KThrowable {
    KObjectHeader header;
    KRef message;
} KThrowable;

static const uint32_t kt_throwable_offsets[] = {offsetof(KThrowable, message)};

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
#define KT_THROWABLE_TYPE(identifier, kotlin_name, base)                                           \
    const KType identifier = {kotlin_name,           sizeof(kotlin_name) - 1,                      \
                              sizeof(KThrowable),    1,                                            \
                              0,                     kt_throwable_offsets,                         \
                              base,                  kt_throwable_vtable,                          \
                              3,                     0};

KT_THROWABLE_TYPE(kt_type_throwable, "kotlin.Throwable", &kt_type_any)
KT_THROWABLE_TYPE(kt_type_error, "kotlin.Error", &kt_type_throwable)
KT_THROWABLE_TYPE(kt_type_not_implemented_error, "kotlin.NotImplementedError", &kt_type_error)
KT_THROWABLE_TYPE(kt_type_exception, "kotlin.Exception", &kt_type_throwable)
KT_THROWABLE_TYPE(kt_type_runtime_exception, "kotlin.RuntimeException", &kt_type_exception)
KT_THROWABLE_TYPE(kt_type_illegal_state_exception, "kotlin.IllegalStateException",
                  &kt_type_runtime_exception)
KT_THROWABLE_TYPE(kt_type_illegal_argument_exception, "kotlin.IllegalArgumentException",
                  &kt_type_runtime_exception)
KT_THROWABLE_TYPE(kt_type_assertion_error, "kotlin.AssertionError", &kt_type_error)
KT_THROWABLE_TYPE(kt_type_null_pointer_exception, "kotlin.NullPointerException",
                  &kt_type_runtime_exception)
KT_THROWABLE_TYPE(kt_type_class_cast_exception, "kotlin.ClassCastException",
                  &kt_type_runtime_exception)
KT_THROWABLE_TYPE(kt_type_index_out_of_bounds_exception, "kotlin.IndexOutOfBoundsException",
                  &kt_type_runtime_exception)
KT_THROWABLE_TYPE(kt_type_arithmetic_exception, "kotlin.ArithmeticException",
                  &kt_type_runtime_exception)
KT_THROWABLE_TYPE(kt_type_unsupported_operation_exception, "kotlin.UnsupportedOperationException",
                  &kt_type_runtime_exception)
/* `NumberFormatException` is Kotlin's `IllegalArgumentException`, not a sibling of it. */
KT_THROWABLE_TYPE(kt_type_number_format_exception, "kotlin.NumberFormatException",
                  &kt_type_illegal_argument_exception)
KT_THROWABLE_TYPE(kt_type_no_such_element_exception, "kotlin.NoSuchElementException",
                  &kt_type_runtime_exception)
KT_THROWABLE_TYPE(kt_type_concurrent_modification_exception,
                  "kotlin.ConcurrentModificationException", &kt_type_runtime_exception)
KT_THROWABLE_TYPE(kt_type_uninitialized_property_access_exception,
                  "kotlin.UninitializedPropertyAccessException", &kt_type_runtime_exception)

KRef kt_throwable_new(const KType *type, KRef message) {
    /* `message` stays in this parameter across the allocation: it is its root. */
    KThrowable *thrown = (KThrowable *)kt_gc_allocate(type, sizeof(KThrowable));
    thrown->message = message;
    return (KRef)thrown;
}

KRef kt_throwable_message(KRef self) { return ((KThrowable *)self)->message; }

/* `assertFailsWith<T> { … }` when the block did not throw what it had to.

   Kotlin's own wording, in two shapes: `message` is a PREFIX followed by ". " when the caller
   supplied one, and `was` is the exception actually caught or NULL when the block completed. The
   class is named by its descriptor, so it reads `kotlin.IllegalStateException` where kotlin-test
   on the JVM reads `class java.lang.IllegalStateException` — the same difference every other
   report on this target already carries, since these are Kotlin's classes and not the JVM's. */
void kt_assert_failed_to_throw(KRef message, const KType *expected, KRef was) {
    KRef text = message == NULL ? kt_string_utf8("", 0)
                                : kt_string_plus(message, kt_string_utf8(". ", 2));
    text = kt_string_plus(text, kt_string_utf8("Expected an exception of class ", 31));
    text = kt_string_plus(text, kt_string_utf8(expected->name, expected->name_length));
    text = kt_string_plus(text, kt_string_utf8(" to be thrown, but was ", 23));
    text = kt_string_plus(text, was == NULL ? kt_string_utf8("completed successfully.", 23)
                                            : kt_to_string(was));
    kt_throw(kt_throwable_new(&kt_type_assertion_error, text));
}

/* Reading a `lateinit` property before anything assigned it. Kotlin's exception and Kotlin's
   wording; the guard is at the READ, which is where kotlinc puts it too, because the field being
   null is the only evidence there is. */
void kt_uninitialized_property(KRef name) {
    KRef message = kt_string_plus(kt_string_utf8("lateinit property ", 18), name);
    message = kt_string_plus(message, kt_string_utf8(" has not been initialized", 25));
    kt_throw(kt_throwable_new(&kt_type_uninitialized_property_access_exception, message));
}

/* An uncaught throw. Until a `try` exists to catch one, every throw is uncaught by construction —
   a file containing a `try` is declined whole — so reporting and exiting here IS the propagation,
   and it is what Kotlin does with an exception nothing handles. The exit code matches the one a
   failed cast already uses, which is the JVM backend's for an abnormal end. */
/* A string LITERAL, interned. Kotlin promises that equal literals are the same object — `"a" ===
   "a"` is true, and a function returning a literal answers the identical string every call — so a
   literal cannot allocate where it is written. `slot` is a static one per distinct text, filled on
   first use and traced from then on.

   The root is registered BEFORE the allocation rather than after: the slot is reachable from that
   moment, holding NULL until the string exists, and a collection triggered by this very allocation
   finds a root it can trace rather than one it has not been told about. */
KRef kt_string_literal(const char *bytes, kt_int length, KRef *slot) {
    if (*slot == NULL) {
        kt_gc_add_global_root((void **)slot);
        *slot = kt_string_utf8(bytes, length);
    }
    return *slot;
}

/* ---- kotlin.test ---------------------------------------------------------------------------- */

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

void kt_assert_equals(KRef expected, KRef actual, KRef message) {
    if (kt_equals(expected, actual)) {
        return;
    }
    /* Built in pieces because rendering either operand may itself allocate, and the text so far
       has to stay reachable across that — the same reason `kt_list_to_string` is a loop over
       `kt_string_plus` rather than a render into one buffer. */
    KRef text = kt_string_plus(kt_assert_prefix(message), kt_string_utf8("Expected <", 10));
    text = kt_string_plus(text, kt_to_string(expected));
    text = kt_string_plus(text, kt_string_utf8(">, actual <", 11));
    text = kt_string_plus(text, kt_to_string(actual));
    kt_assert_fail(kt_string_plus(text, kt_string_utf8(">.", 2)));
}

/* `assertSame`/`assertNotSame`: IDENTITY, which is the whole of what separates them from
   `assertEquals` — two strings with the same text are equal and are not the same object. Kotlin's
   own wording for each, verified against the reference toolchain.

   Built in pieces for the reason `assertEquals` is: rendering either operand allocates, and the
   text so far has to stay reachable across that. */
void kt_assert_same(KRef expected, KRef actual, KRef message) {
    if (expected == actual) {
        return;
    }
    KRef text = kt_string_plus(kt_assert_prefix(message), kt_string_utf8("Expected <", 10));
    text = kt_string_plus(text, kt_to_string(expected));
    text = kt_string_plus(text, kt_string_utf8(">, actual <", 11));
    text = kt_string_plus(text, kt_to_string(actual));
    kt_assert_fail(kt_string_plus(text, kt_string_utf8("> is not same.", 14)));
}

void kt_assert_not_same(KRef illegal, KRef actual, KRef message) {
    if (illegal != actual) {
        return;
    }
    KRef text = kt_string_plus(kt_assert_prefix(message), kt_string_utf8("Illegal value: <", 16));
    text = kt_string_plus(text, kt_to_string(actual));
    kt_assert_fail(kt_string_plus(text, kt_string_utf8(">.", 2)));
}

void kt_assert_true(kt_boolean actual, KRef message) {
    if (actual) {
        return;
    }
    kt_assert_fail(
        kt_string_plus(kt_assert_prefix(message), kt_string_utf8("Expected value to be true.", 26)));
}

void kt_assert_false(kt_boolean actual, KRef message) {
    if (!actual) {
        return;
    }
    kt_assert_fail(kt_string_plus(kt_assert_prefix(message),
                                  kt_string_utf8("Expected value to be false.", 27)));
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
    /* Equal references must hash alike, so the hash is built from exactly what equality reads. */
    kt_int hash = (kt_int)(uintptr_t)self->header.type->reference_target;
    KRef receiver = kt_reference_receiver(self);
    if (receiver != NULL) {
        hash = hash * 31 + kt_hash_code(receiver);
    }
    return hash;
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
   branch is also what "How an exception propagates" in `docs/BUILD_AND_NATIVE_PLAN.md` costed the
   design at. */
KRef kt_pending;
static kt_boolean kt_pending_root;

static void kt_pending_register(void) {
    if (!kt_pending_root) {
        kt_pending_root = 1;
        kt_gc_add_global_root((void **)&kt_pending);
    }
}

/* `throw e`: record it and RETURN. The caller's next act is the check below, which is what turns
   the return into propagation; see "How an exception propagates" in `docs/BUILD_AND_NATIVE_PLAN.md`
   for why this rather than unwind tables or `setjmp`. */
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
   program's `main` and is about to treat its answer as an answer. */
void kt_check_uncaught(void) {
    if (kt_pending == NULL) {
        return;
    }
    kt_int length = 0;
    KRef storage = NULL;
    const char *bytes = kt_render(kt_pending, &length, &storage);
    kt_write(2, "Exception in thread \"main\" ", 27);
    kt_write(2, bytes, (size_t)length);
    kt_write(2, "\n", 1);
    kt_sys_exit(134);
}

static void kt_emit(KRef value, bool newline) {
    kt_int length = 0;
    KRef storage = NULL;
    const char *bytes = kt_render(value, &length, &storage);
    kt_write(1, bytes, (size_t)length);
    if (newline) {
        kt_write(1, "\n", 1);
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

void kt_println_unit(void) { kt_write(1, "\n", 1); }
