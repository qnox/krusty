/* `next()` on an exhausted array or string iterator raises `NoSuchElementException`, as Kotlin's
   does. The general `next` used to read the element past the end without asking: an array's came
   back as whatever followed the storage, boxed, with nothing raised, and a string's raised the
   `IndexOutOfBoundsException` its indexed read raises instead. */
#include "collections_later_tiers.h"

void kt_program_entry(void) {
    int stack_bottom;
    kt_runtime_init(&stack_bottom);
    KRef references = kt_array_new(&kt_type_array, 1);
    ((KRef *)((KArray *)references + 1))[0] = kt_box_int(5);
    KRef walk = kt_iterable_iterator(references);
    CHECK(kt_unbox_int(kt_iterator_next(walk)) == 5, "the array walk lost its element\n");
    CHECK(!kt_iterator_has_next(walk), "the array walk found a second element\n");
    CHECK(kt_iterator_next(walk) == NULL, "an exhausted array walk answered an element\n");
    CHECK(took(&kt_type_no_such_element_exception), "an exhausted array walk raised no NSEE\n");

    KRef ints = kt_array_new(&kt_type_int_array, 1);
    walk = kt_iterable_iterator(ints);
    CHECK(kt_unbox_int(kt_iterator_next(walk)) == 0, "the IntArray walk lost its element\n");
    CHECK(kt_iterator_next(walk) == NULL, "an exhausted IntArray walk answered an element\n");
    CHECK(took(&kt_type_no_such_element_exception), "an exhausted IntArray walk raised no NSEE\n");

    walk = kt_iterable_iterator(kt_string_utf8("a", 1));
    CHECK(kt_unbox_char(kt_iterator_next(walk)) == 'a', "the string walk lost its character\n");
    CHECK(kt_iterator_next(walk) == NULL, "an exhausted string walk answered a character\n");
    CHECK(took(&kt_type_no_such_element_exception), "an exhausted string walk raised no NSEE\n");
    kt_sys_write(1, "OK\n", 3);
}
