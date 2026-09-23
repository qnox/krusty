/* A progression's `toString`, `equals` and `hashCode` include its STEP, as Kotlin's
   `IntProgression`, `LongProgression` and `CharProgression` do; only the ranges `..` and `until`
   answer (`IntRange` and its kin) leave it out. The runtime keeps both in one struct, and the three
   members read only the bounds, so `1..10 step 2` printed `1..9`, `10 downTo 1` printed `10..1`
   and equalled `10..1`, and two progressions differing only by step were equal. */
#include "krusty_rt.h"
#include "krusty_sys.h"

#define CHECK(condition, literal)                                                                  \
    do {                                                                                           \
        if (!(condition)) {                                                                        \
            KT_SYS_FAIL("range_progression_members: " literal "\n");                               \
        }                                                                                          \
    } while (0)

/* The three `kotlin.Any` members, through the table the way a virtual call reaches them. */
static kt_boolean equals(KRef self, KRef other) {
    const KType *type = ((const KObjectHeader *)self)->type;
    return ((kt_boolean(*)(KRef, KRef))type->vtable[KT_SLOT_EQUALS])(self, other);
}

static kt_int hash_code(KRef self) {
    const KType *type = ((const KObjectHeader *)self)->type;
    return ((kt_int(*)(KRef))type->vtable[KT_SLOT_HASH_CODE])(self);
}

/* Whether `value.toString()` is exactly these bytes. Two prefix tests rather than an equality,
   because the string layout is the runtime's own. */
static kt_boolean renders(KRef value, const char *bytes) {
    const KType *type = ((const KObjectHeader *)value)->type;
    KRef text = ((KRef(*)(KRef))type->vtable[KT_SLOT_TO_STRING])(value);
    kt_int length = 0;
    while (bytes[length] != 0) {
        length++;
    }
    KRef expected = kt_string_utf8(bytes, length);
    return kt_string_starts_with(text, expected) && kt_string_starts_with(expected, text);
}

void kt_program_entry(void) {
    int stack_bottom;
    kt_runtime_init(&stack_bottom);

    KRef one_to_ten = kt_int_range(1, 10);
    KRef odd = kt_range_step(one_to_ten, 2);
    KRef ten_down = kt_int_range_down_to(10, 1);
    KRef ten_to_one = kt_int_range(10, 1);

    /* `toString`: a range is its bounds, a progression names its step, and a descending one is
       written the way it was built. */
    CHECK(renders(one_to_ten, "1..10"), "1..10 renders as 1..10");
    CHECK(renders(odd, "1..9 step 2"), "1..10 step 2 renders as 1..9 step 2");
    CHECK(renders(ten_down, "10 downTo 1 step 1"), "10 downTo 1 renders as 10 downTo 1 step 1");
    CHECK(renders(kt_range_step(kt_int_range(1, 3), 1), "1..3 step 1"),
          "1..3 step 1 renders as 1..3 step 1");
    CHECK(renders(kt_range_reversed(kt_range_step(kt_int_range(1, 9), 3)), "7 downTo 1 step 3"),
          "(1..9 step 3).reversed() renders as 7 downTo 1 step 3");
    CHECK(renders(kt_range_step(kt_long_range_down_to(5, 1), 2), "5 downTo 1 step 2"),
          "5L downTo 1L step 2 renders as 5 downTo 1 step 2");
    CHECK(renders(kt_range_step(kt_char_range('a', 'e'), 2), "a..e step 2"),
          "'a'..'e' step 2 renders as a..e step 2");
    CHECK(renders(kt_char_range_down_to('e', 'a'), "e downTo a step 1"),
          "'e' downTo 'a' renders as e downTo a step 1");

    /* `equals`: a progression's includes the step; a range equals only a range, while a
       progression equals a range of the same walk. Two empty ones of a kind are equal. */
    CHECK(!equals(ten_down, ten_to_one), "(10 downTo 1) == (10..1)");
    CHECK(!equals(ten_to_one, ten_down), "(10..1) == (10 downTo 1)");
    CHECK(!equals(kt_range_step(kt_int_range(1, 9), 2), kt_range_step(kt_int_range(1, 9), 4)),
          "(1..9 step 2) == (1..9 step 4)");
    CHECK(equals(odd, kt_range_step(kt_int_range(1, 9), 2)), "(1..10 step 2) != (1..9 step 2)");
    CHECK(equals(kt_range_step(kt_int_range(1, 3), 1), kt_int_range(1, 3)),
          "(1..3 step 1) != (1..3)");
    CHECK(!equals(kt_int_range(1, 3), kt_range_step(kt_int_range(1, 3), 1)),
          "(1..3) == (1..3 step 1)");
    CHECK(equals(kt_int_range_down_to(1, 10), kt_int_range(5, 1)), "(1 downTo 10) != (5..1)");
    CHECK(!equals(kt_int_range(5, 1), kt_int_range_down_to(1, 10)), "(5..1) == (1 downTo 10)");
    CHECK(equals(kt_int_range(1, 3), kt_int_range(1, 3)), "(1..3) != (1..3)");

    /* `hashCode`: `31 * (31 * first + last) + step` for a progression, each part folded through
       its own type's hash, and `31 * first + last` for a range. */
    CHECK(hash_code(one_to_ten) == 41, "(1..10).hashCode()");
    CHECK(hash_code(odd) == 1242, "(1..10 step 2).hashCode()");
    CHECK(hash_code(ten_down) == 9640, "(10 downTo 1).hashCode()");
    CHECK(hash_code(kt_range_step(kt_long_range_down_to(5, 1), 2)) == 4837,
          "(5L downTo 1L step 2).hashCode()");
    CHECK(hash_code(kt_range_step(kt_char_range('a', 'e'), 2)) == 96350,
          "('a'..'e' step 2).hashCode()");
    CHECK(hash_code(kt_int_range_down_to(1, 10)) == -1, "(1 downTo 10).hashCode()");

    kt_sys_write(1, "OK\n", 3);
}
