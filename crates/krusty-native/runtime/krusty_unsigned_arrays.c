/* Unsigned array value classes: nominal views retaining their signed backing arrays. */
#include "krusty_internal.h"

/* An unsigned array is a nominal value-class object retaining its original signed array. The
   length prefix keeps the array protocol uniform; the backing reference is traced as an ordinary
   field, rather than pretending the view contains inline elements. */
typedef struct KUnsignedArrayView {
    KArray array;
    KRef storage;
} KUnsignedArrayView;

static const uint32_t kt_unsigned_array_references[] = {
    offsetof(KUnsignedArrayView, storage)};

const KType *kt_unsigned_array_carrier(const KType *type) {
    if (type == &kt_type_ubyte_array) {
        return &kt_type_byte_array;
    }
    if (type == &kt_type_ushort_array) {
        return &kt_type_short_array;
    }
    if (type == &kt_type_uint_array) {
        return &kt_type_int_array;
    }
    if (type == &kt_type_ulong_array) {
        return &kt_type_long_array;
    }
    return NULL;
}

KRef kt_array_storage(KRef array) {
    return array != NULL && kt_unsigned_array_carrier(array->header.type) != NULL
               ? ((const KUnsignedArrayView *)array)->storage
               : array;
}

KRef kt_unsigned_array_wrap(const KType *type, KRef storage) {
    const KType *carrier = kt_unsigned_array_carrier(type);
    if (carrier == NULL || storage == NULL || storage->header.type != carrier) {
        KT_FAIL("krusty: an unsigned array view requires its exact signed carrier\n");
    }
    /* A collection while allocating the view must still see the owning array. The volatile
       slot keeps the reference on the conservatively scanned stack until the view holds it. */
    KRef volatile rooted_storage = storage;
    KUnsignedArrayView *view = (KUnsignedArrayView *)kt_gc_allocate(type, sizeof(*view));
    view->storage = rooted_storage;
    view->array.length = ((const KArray *)view->storage)->length;
    return (KRef)view;
}

static kt_boolean kt_unsigned_array_equals(KRef self, KRef other) {
    return other != NULL && self->header.type == other->header.type
           && kt_array_storage(self) == kt_array_storage(other);
}

static kt_int kt_unsigned_array_hash_code(KRef self) {
    return kt_array_content_hash_code(kt_array_storage(self));
}

/* The generated value-class rendering uses the SIGNED storage's contents. */
static KRef kt_unsigned_array_to_string(KRef self) {
    const KType *type = self->header.type;
    uint32_t start = type->name_length;
    while (start > 0 && type->name[start - 1] != '.') {
        start--;
    }
    KRef builder = kt_string_builder_new();
    kt_string_builder_append(builder,
                             kt_string_utf8(type->name + start, (kt_int)(type->name_length - start)));
    kt_string_builder_append(builder, kt_string_utf8("(storage=", 9));
    kt_string_builder_append(builder, kt_array_content_to_string(kt_array_storage(self)));
    kt_string_builder_append(builder, kt_string_utf8(")", 1));
    return kt_to_string(builder);
}

static const kt_fn kt_unsigned_array_vtable[3] = {
    (kt_fn)kt_unsigned_array_equals, (kt_fn)kt_unsigned_array_hash_code,
    (kt_fn)kt_unsigned_array_to_string};

#define KT_UNSIGNED_ARRAY_TYPE(identifier, simple)                                                 \
    const KType identifier = {KT_NAMED("kotlin.", simple),                                         \
                              .instance_size = sizeof(KUnsignedArrayView),                          \
                              .reference_count = 1,                                                \
                              .reference_offsets = kt_unsigned_array_references,                  \
                              .super = &kt_type_any,                                               \
                              .vtable = kt_unsigned_array_vtable,                                  \
                              .vtable_length = 3,                                                  \
                              .element_references = 0};

KT_UNSIGNED_ARRAY_TYPE(kt_type_ubyte_array, "UByteArray")
KT_UNSIGNED_ARRAY_TYPE(kt_type_ushort_array, "UShortArray")
KT_UNSIGNED_ARRAY_TYPE(kt_type_uint_array, "UIntArray")
KT_UNSIGNED_ARRAY_TYPE(kt_type_ulong_array, "ULongArray")
