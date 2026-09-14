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

KRef kt_array_new(const KType *type, kt_int length) {
    if (length < 0) {
        KT_FAIL("krusty: negative array size\n");
    }
    KArray *array = (KArray *)kt_gc_allocate(
        type, (uint32_t)sizeof(KArray) + (uint32_t)length * type->element_size);
    array->length = length;
    return (KRef)array;
}

void kt_index_out_of_bounds(kt_int index, kt_int size) {
    (void)index;
    (void)size;
    KT_FAIL("krusty: array index out of bounds\n");
}

/* `Color.valueOf("NOPE")` throws in Kotlin; a program without exceptions stops here instead. The
   name is taken so the failure can say which one was asked for once strings can be rendered from
   the runtime's own failure path. */
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

void kt_no_such_enum_constant(KRef name) {
    (void)name;
    KT_FAIL("krusty: no enum constant of that name\n");
}

typedef KArray KByteArray;

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

KT_TYPE(kt_type_string, "kotlin.String", sizeof(KObject), 1, kt_string_references)
KT_TYPE(kt_type_byte, "kotlin.Byte", sizeof(KObject), 0, NULL)
KT_TYPE(kt_type_short, "kotlin.Short", sizeof(KObject), 0, NULL)
KT_TYPE(kt_type_int, "kotlin.Int", sizeof(KObject), 0, NULL)
KT_TYPE(kt_type_long, "kotlin.Long", sizeof(KObject), 0, NULL)
KT_TYPE(kt_type_char, "kotlin.Char", sizeof(KObject), 0, NULL)
KT_TYPE(kt_type_boolean, "kotlin.Boolean", sizeof(KObject), 0, NULL)
KT_TYPE(kt_type_float, "kotlin.Float", sizeof(KObject), 0, NULL)
KT_TYPE(kt_type_double, "kotlin.Double", sizeof(KObject), 0, NULL)
KT_TYPE(kt_type_unit, "kotlin.Unit", sizeof(KObject), 0, NULL)

/* Kotlin's four unsigned integers. Each is a value class over a signed primitive, and the generated
   code carries it as the machine integer it wraps — the right machine shape, and the wrong one to
   ask questions of, since `4294967295u` is that `Int`'s bits and not its value. A descriptor of its
   own is what keeps `1u as? Int` false and makes a boxed one render its value; the bits live in the
   signed field of the matching width, and only the descriptor says how to read them. */
KT_TYPE(kt_type_ubyte, "kotlin.UByte", sizeof(KObject), 0, NULL)
KT_TYPE(kt_type_ushort, "kotlin.UShort", sizeof(KObject), 0, NULL)
KT_TYPE(kt_type_uint, "kotlin.UInt", sizeof(KObject), 0, NULL)
KT_TYPE(kt_type_ulong, "kotlin.ULong", sizeof(KObject), 0, NULL)

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

/* Kotlin's `String.length` counts UTF-16 CODE UNITS; a krusty string holds UTF-8. The byte length
   is therefore not the answer, and neither is the code-point count. In UTF-8 a byte that is not a
   continuation byte (`10xxxxxx`) starts exactly one code point, so counting those counts code
   points; of those, only the ones a four-byte sequence starts (`11110xxx`, i.e. above U+FFFF) are
   written as a SURROGATE PAIR in UTF-16 and contribute two units. Everything else contributes one.

   This walks the bytes on every call, which is what a string that stores UTF-8 costs; it is also
   what makes the answer right for text a JVM-shaped length would have to be stored alongside. */
kt_int kt_string_length(KRef self) {
    const char *bytes = self->as.string.bytes;
    kt_int byte_length = self->as.string.byte_length;
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
    const char *bytes = self->as.string.bytes;
    kt_int byte_length = self->as.string.byte_length;
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
        if ((int)value >= (low) && (int)value <= (high)) {                                         \
            KObject *cached = &kt_cache_##suffix[(int)value - (low)];                              \
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

/* Unboxing a `null` is Kotlin's `NullPointerException`. With no exception machinery yet, the
   honest realization is a diagnosable exit rather than a silent zero. */
#define KT_UNBOX(suffix, field, type)                                                              \
    type kt_unbox_##suffix(KRef value) {                                                           \
        if (value == NULL) {                                                                       \
            KT_FAIL("krusty: null cannot be cast to a non-null type\n");                           \
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


static KRef kt_range_new(const KType *type, kt_long first, kt_long last) {
    KRange *range = (KRange *)kt_gc_allocate(type, sizeof(KRange));
    range->first = first;
    range->last = last;
    return (KRef)range;
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

static kt_boolean kt_range_empty(const KRange *range) { return range->first > range->last; }

kt_boolean kt_range_is_empty(KRef range) { return kt_range_empty((const KRange *)range); }

kt_long kt_range_first(KRef range) { return ((const KRange *)range)->first; }

kt_long kt_range_last(KRef range) { return ((const KRange *)range)->last; }

/* `value in range`. The bounds were stored at 64 bits with the element type's own signedness, so
   the caller widens the same way and one comparison serves all three. */
kt_boolean kt_range_contains(KRef range, kt_long value) {
    const KRange *self = (const KRange *)range;
    return self->first <= value && value <= self->last;
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
    if (self->header.type == &kt_type_long_range) {
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
    kt_boolean has_next;
} KRangeIterator;

KT_RANGE_TYPE(kt_type_int_iterator, "kotlin.collections.IntIterator", KRangeIterator, kt_any_vtable)
KT_RANGE_TYPE(kt_type_long_iterator, "kotlin.collections.LongIterator", KRangeIterator, kt_any_vtable)
KT_RANGE_TYPE(kt_type_char_iterator, "kotlin.collections.CharIterator", KRangeIterator, kt_any_vtable)

KRef kt_range_iterator(KRef range) {
    const KRange *bounds = (const KRange *)range;
    const KType *type = range->header.type == &kt_type_long_range   ? &kt_type_long_iterator
                        : range->header.type == &kt_type_char_range ? &kt_type_char_iterator
                                                                    : &kt_type_int_iterator;
    KRangeIterator *iterator = (KRangeIterator *)kt_gc_allocate(type, sizeof(KRangeIterator));
    iterator->next = bounds->first;
    iterator->last = bounds->last;
    iterator->has_next = bounds->first <= bounds->last;
    return (KRef)iterator;
}

kt_boolean kt_range_iterator_has_next(KRef iterator) {
    return ((const KRangeIterator *)iterator)->has_next;
}

kt_long kt_range_iterator_next(KRef iterator) {
    KRangeIterator *self = (KRangeIterator *)iterator;
    if (!self->has_next) {
        KT_FAIL("krusty: no more elements in this range\n");
    }
    kt_long value = self->next;
    if (value == self->last) {
        self->has_next = false;
    } else {
        self->next = value + 1;
    }
    return value;
}

#undef KT_RANGE_TYPE

/* `"$first..$last"`, with a `CharRange`'s bounds rendered as the characters they are. */
static KRef kt_range_to_string(KRef self) {
    const KRange *range = (const KRange *)self;
    kt_boolean chars = self->header.type == &kt_type_char_range;
    KByteArray *buffer = kt_bytes_new(48);
    char *out = kt_bytes_of(buffer);
    kt_int length =
        chars ? kt_render_char((kt_char)range->first, out) : kt_render_long(range->first, out);
    out[length++] = '.';
    out[length++] = '.';
    length += chars ? kt_render_char((kt_char)range->last, out + length)
                    : kt_render_long(range->last, out + length);
    return kt_string_of((KRef)buffer, kt_bytes_of(buffer), length);
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

/* The cursor an iterator holds is an INDEX, not a pointer: the collector may not move an object,
   but an index needs no such promise and reads the same whatever the list is. */
typedef struct KListIterator {
    KObjectHeader header;
    KRef list;
    kt_int at;
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

static kt_int kt_length_of(KRef array) { return ((const KArray *)array)->length; }

KRef kt_list_of(KRef elements) {
    /* The allocation can collect, so the array has to be reachable across it; it is, in this
       local, which the conservative root scan reads. */
    KList *list = (KList *)kt_gc_allocate(&kt_type_list, sizeof(KList));
    list->elements = elements;
    return (KRef)list;
}

KRef kt_list_empty(void) { return kt_list_of(kt_array_new(&kt_type_array, 0)); }

kt_int kt_list_size(KRef list) { return kt_length_of(((const KList *)list)->elements); }

kt_boolean kt_list_is_empty(KRef list) { return kt_list_size(list) == 0; }

KRef kt_list_get(KRef list, kt_int index) {
    KRef elements = ((const KList *)list)->elements;
    if (index < 0 || index >= kt_length_of(elements)) {
        kt_index_out_of_bounds(index, kt_length_of(elements));
    }
    return kt_elements_of(elements)[index];
}

kt_int kt_list_index_of(KRef list, KRef value) {
    KRef elements = ((const KList *)list)->elements;
    kt_int length = kt_length_of(elements);
    for (kt_int i = 0; i < length; i++) {
        if (kt_equals(kt_elements_of(elements)[i], value)) {
            return i;
        }
    }
    return -1;
}

kt_int kt_list_last_index_of(KRef list, KRef value) {
    KRef elements = ((const KList *)list)->elements;
    for (kt_int i = kt_length_of(elements) - 1; i >= 0; i--) {
        if (kt_equals(kt_elements_of(elements)[i], value)) {
            return i;
        }
    }
    return -1;
}

kt_boolean kt_list_contains(KRef list, KRef value) { return kt_list_index_of(list, value) >= 0; }

KRef kt_list_iterator(KRef list) {
    KListIterator *iterator =
        (KListIterator *)kt_gc_allocate(&kt_type_list_iterator, sizeof(KListIterator));
    iterator->list = list;
    iterator->at = 0;
    return (KRef)iterator;
}

kt_boolean kt_list_iterator_has_next(KRef iterator) {
    const KListIterator *self = (const KListIterator *)iterator;
    return self->at < kt_list_size(self->list);
}

KRef kt_list_iterator_next(KRef iterator) {
    KListIterator *self = (KListIterator *)iterator;
    if (self->at >= kt_list_size(self->list)) {
        KT_FAIL("krusty: no more elements in this list\n");
    }
    return kt_list_get(self->list, self->at++);
}

/* An `Iterable` or an `Iterator` that the generator could only type by the INTERFACE — a generic
   body, an inlined stdlib extension — may be holding either of the two things this runtime can
   iterate. The static type cannot say which, so the descriptor does.

   `next` answers a reference for the same reason the question arises: a receiver typed by the
   interface has its element type erased, so what a caller there expects is the boxed element. */
KRef kt_iterable_iterator(KRef iterable) {
    if (iterable != NULL && iterable->header.type == &kt_type_list) {
        return kt_list_iterator(iterable);
    }
    return kt_range_iterator(iterable);
}

kt_boolean kt_iterator_has_next(KRef iterator) {
    if (iterator != NULL && iterator->header.type == &kt_type_list_iterator) {
        return kt_list_iterator_has_next(iterator);
    }
    return kt_range_iterator_has_next(iterator);
}

KRef kt_iterator_next(KRef iterator) {
    if (iterator == NULL) {
        KT_FAIL("krusty: member access on a null receiver\n");
    }
    if (iterator->header.type == &kt_type_list_iterator) {
        return kt_list_iterator_next(iterator);
    }
    kt_long value = kt_range_iterator_next(iterator);
    if (iterator->header.type == &kt_type_long_iterator) {
        return kt_box_long(value);
    }
    if (iterator->header.type == &kt_type_char_iterator) {
        return kt_box_char((kt_char)value);
    }
    return kt_box_int((kt_int)value);
}

/* Kotlin's `List.equals`: same size and elementwise equal, and only against another list. A list
   never equals a set with the same members, which the type comparison is. */
static kt_boolean kt_list_equals(KRef self, KRef other) {
    if (self == other) {
        return true;
    }
    if (other == NULL || other->header.type != &kt_type_list) {
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
    kt_int length = kt_length_of(elements);
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
    kt_int length = kt_length_of(elements);
    KRef text = kt_string_utf8("[", 1);
    for (kt_int i = 0; i < length; i++) {
        if (i > 0) {
            text = kt_string_plus(text, kt_string_utf8(", ", 2));
        }
        text = kt_string_plus(text, kt_to_string(kt_elements_of(elements)[i]));
    }
    return kt_string_plus(text, kt_string_utf8("]", 1));
}

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
        uint64_t left, right;
        memcpy(&left, &self->as.double_value, sizeof left);
        memcpy(&right, &other->as.double_value, sizeof right);
        return left == right;
    }
    if (type == &kt_type_float) {
        uint32_t left, right;
        memcpy(&left, &self->as.float_value, sizeof left);
        memcpy(&right, &other->as.float_value, sizeof right);
        return left == right;
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
        uint64_t bits;
        memcpy(&bits, &self->as.double_value, sizeof bits);
        return (kt_int)(uint32_t)(bits ^ (bits >> 32));
    }
    if (type == &kt_type_float) {
        uint32_t bits;
        memcpy(&bits, &self->as.float_value, sizeof bits);
        return (kt_int)bits;
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

static void kt_fail_cast(KRef object, const KType *type) {
    kt_write(2, "krusty: class ", 14);
    if (object == NULL) {
        kt_write(2, "null", 4);
    } else {
        kt_write(2, object->header.type->name, object->header.type->name_length);
    }
    kt_write(2, " cannot be cast to ", 19);
    kt_write(2, type->name, type->name_length);
    kt_write(2, "\n", 1);
    kt_sys_exit(134);
}

KRef kt_cast(KRef object, const KType *type) {
    if (object != NULL && !kt_is_instance(object, type)) {
        kt_fail_cast(object, type);
    }
    return object;
}

KRef kt_cast_non_null(KRef object, const KType *type) {
    if (!kt_is_instance(object, type)) {
        kt_fail_cast(object, type);
    }
    return object;
}

KRef kt_safe_cast(KRef object, const KType *type) {
    return kt_is_instance(object, type) ? object : NULL;
}

KRef kt_not_null(KRef value) {
    if (value == NULL) {
        KT_FAIL("krusty: null cannot be cast to a non-null type\n");
    }
    return value;
}

void kt_abstract_method_called(void) { KT_FAIL("krusty: abstract method called\n"); }

void kt_null_receiver(void) { KT_FAIL("krusty: member access on a null receiver\n"); }

/* A callable whose Kotlin type is `Nothing` returned instead of diverging, and the path that reads
   its value has no value to read. The JVM throws `KotlinNothingValueException` here; there are no
   exceptions on this target yet, so the failure is the loud one — and no program that could catch
   it compiles here anyway. */
void kt_nothing_value_returned(void) {
    KT_FAIL("krusty: a `Nothing`-typed callable returned a value\n");
}

/* ---- arithmetic ---------------------------------------------------------------------------- */

static void kt_divide_by_zero(void) { KT_FAIL("krusty: / by zero\n"); }

kt_int kt_div_int(kt_int a, kt_int b) {
    if (b == 0) {
        kt_divide_by_zero();
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
    }
    if (b == -1) {
        return 0;
    }
    return a % b;
}

kt_long kt_div_long(kt_long a, kt_long b) {
    if (b == 0) {
        kt_divide_by_zero();
    }
    if (b == -1) {
        return (kt_long)(0u - (uint64_t)a);
    }
    return a / b;
}

kt_long kt_rem_long(kt_long a, kt_long b) {
    if (b == 0) {
        kt_divide_by_zero();
    }
    if (b == -1) {
        return 0;
    }
    return a % b;
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
        KT_FAIL("krusty: null cannot be cast to a non-null type\n");
    }
    return value->as.byte_value;
}

kt_short kt_unbox_ushort(KRef value) {
    if (value == NULL) {
        KT_FAIL("krusty: null cannot be cast to a non-null type\n");
    }
    return value->as.short_value;
}

kt_int kt_unbox_uint(KRef value) {
    if (value == NULL) {
        KT_FAIL("krusty: null cannot be cast to a non-null type\n");
    }
    return value->as.int_value;
}

kt_long kt_unbox_ulong(KRef value) {
    if (value == NULL) {
        KT_FAIL("krusty: null cannot be cast to a non-null type\n");
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
    }
    return (kt_int)((uint32_t)a / (uint32_t)b);
}

kt_int kt_rem_uint(kt_int a, kt_int b) {
    if (b == 0) {
        kt_divide_by_zero();
    }
    return (kt_int)((uint32_t)a % (uint32_t)b);
}

kt_long kt_div_ulong(kt_long a, kt_long b) {
    if (b == 0) {
        kt_divide_by_zero();
    }
    return (kt_long)((uint64_t)a / (uint64_t)b);
}

kt_long kt_rem_ulong(kt_long a, kt_long b) {
    if (b == 0) {
        kt_divide_by_zero();
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
