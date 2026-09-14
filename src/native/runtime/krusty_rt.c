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
    if (type == &kt_type_byte) {
        return self->as.byte_value == other->as.byte_value;
    }
    if (type == &kt_type_short) {
        return self->as.short_value == other->as.short_value;
    }
    if (type == &kt_type_int) {
        return self->as.int_value == other->as.int_value;
    }
    if (type == &kt_type_long) {
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
    if (type == &kt_type_byte) {
        return (kt_int)self->as.byte_value;
    }
    if (type == &kt_type_short) {
        return (kt_int)self->as.short_value;
    }
    if (type == &kt_type_int) {
        return self->as.int_value;
    }
    if (type == &kt_type_long) {
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

#undef KT_CONSOLE

void kt_println_unit(void) { kt_write(1, "\n", 1); }
