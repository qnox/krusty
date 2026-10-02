/* What each iterator the runtime hands out IS: its class, its superclass, and which of
   `kotlin.collections.Iterator` and the abstract primitive iterators (`IntIterator` and kin) an `is`
   finds it to be. A progression's iterator subclassed `IntIterator` (or kin) but implemented no
   `Iterator`, so `it is Iterator<*>` was false; and every primitive array's iterator was the one
   generic array iterator, so `intArrayOf(1).iterator() is IntIterator` was false.

   The driver prints each iterator's identity, and the harness compares the lines with what
   `iterator_identity.kt` answers under the reference kotlinc. An array's or a list's iterator is
   Kotlin/Native's class, a declared divergence: the JVM's `intArrayOf(1).iterator()` is
   `kotlin.jvm.internal.ArrayIntIterator`, Kotlin/Native's `kotlin.IntArrayIterator`; the JVM's
   `listOf(1, 2).iterator()` is `java.util.Arrays.ArrayItr`, Kotlin/Native's the anonymous one
   `Array.asList` makes, with no qualified name. */
#include "collections_later_tiers.h"
#include "transcript.h"

/* The abstract primitive iterators, which the runtime defines and a program's `is` names. */
extern const KType kt_type_boolean_iterator;
extern const KType kt_type_byte_iterator;
extern const KType kt_type_char_iterator;
extern const KType kt_type_short_iterator;
extern const KType kt_type_int_iterator;
extern const KType kt_type_long_iterator;
extern const KType kt_type_float_iterator;
extern const KType kt_type_double_iterator;

static const struct {
    const char *name;
    const KType *type;
} kinds[] = {
    {"Iterator", &kt_type_iterator_interface},
    {"BooleanIterator", &kt_type_boolean_iterator},
    {"ByteIterator", &kt_type_byte_iterator},
    {"CharIterator", &kt_type_char_iterator},
    {"ShortIterator", &kt_type_short_iterator},
    {"IntIterator", &kt_type_int_iterator},
    {"LongIterator", &kt_type_long_iterator},
    {"FloatIterator", &kt_type_float_iterator},
    {"DoubleIterator", &kt_type_double_iterator},
};

/* `label: <name> super=<superclass> is <kinds>`. */
static void show(const char *label, KRef iterator) {
    KRef literal = kt_class_of(iterator);
    say(label);
    say(": ");
    say_value(kt_class_qualified_name(literal));
    const KType *parent = type_of(iterator)->super;
    CHECK(parent != NULL, "an iterator class has no superclass\n");
    say(" super=");
    say_bytes(parent->name, (kt_int)parent->name_length);
    say(" is");
    for (unsigned at = 0; at < sizeof(kinds) / sizeof(kinds[0]); at++) {
        if (kt_is_instance(iterator, kinds[at].type)) {
            say(" ");
            say(kinds[at].name);
        }
    }
    say("\n");
}

static KRef array_iterator(const KType *type) {
    return kt_iterable_iterator(kt_array_new(type, 1));
}

void kt_program_entry(void) {
    DRIVER_BEGIN();
    show("(1..2).iterator()", kt_range_iterator(kt_int_range(1, 2)));
    show("(2 downTo 1).iterator()", kt_range_iterator(kt_int_range_down_to(2, 1)));
    show("(1L..2L).iterator()", kt_range_iterator(kt_long_range(1, 2)));
    show("('a'..'b').iterator()", kt_range_iterator(kt_char_range('a', 'b')));
    show("(1u..2u).iterator()", kt_range_iterator(kt_uint_range(1, 2)));
    show("(1uL..2uL).iterator()", kt_range_iterator(kt_ulong_range(1, 2)));
    show("arrayOf(1).iterator()", array_iterator(&kt_type_array));
    show("BooleanArray(1).iterator()", array_iterator(&kt_type_boolean_array));
    show("ByteArray(1).iterator()", array_iterator(&kt_type_byte_array));
    show("CharArray(1).iterator()", array_iterator(&kt_type_char_array));
    show("ShortArray(1).iterator()", array_iterator(&kt_type_short_array));
    show("IntArray(1).iterator()", array_iterator(&kt_type_int_array));
    show("LongArray(1).iterator()", array_iterator(&kt_type_long_array));
    show("FloatArray(1).iterator()", array_iterator(&kt_type_float_array));
    show("DoubleArray(1).iterator()", array_iterator(&kt_type_double_array));
    show("UByteArray(1).iterator()", array_iterator(&kt_type_ubyte_array));
    show("UShortArray(1).iterator()", array_iterator(&kt_type_ushort_array));
    show("UIntArray(1).iterator()", array_iterator(&kt_type_uint_array));
    show("ULongArray(1).iterator()", array_iterator(&kt_type_ulong_array));
    show("\"ab\".iterator()", kt_iterable_iterator(kt_string_utf8("ab", 2)));
    show("StringBuilder(\"ab\").iterator()",
         kt_iterable_iterator(kt_string_builder_with_text(kt_string_utf8("ab", 2))));

    KRef two = kt_array_new(&kt_type_array, 2);
    ((KRef *)((KArray *)two + 1))[0] = kt_box_int(1);
    ((KRef *)((KArray *)two + 1))[1] = kt_box_int(2);
    show("listOf(1, 2).iterator()", kt_iterable_iterator(kt_list_of(two)));
    show("mutableListOf(1, 2).iterator()", kt_iterable_iterator(kt_mutable_list_of(two)));
    CHECK(kt_pending_exception() == NULL, "making an iterator raised\n");
    kt_sys_write(1, "OK\n", 3);
}
