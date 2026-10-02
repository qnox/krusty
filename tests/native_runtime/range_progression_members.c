/* A progression's `toString`, `equals` and `hashCode` include its STEP, as Kotlin's
   `IntProgression`, `LongProgression` and `CharProgression` do; only the ranges `..` and `until`
   answer (`IntRange` and its kin) leave it out. The runtime keeps both in one struct, and the three
   members read only the bounds, so `1..10 step 2` printed `1..9`, `10 downTo 1` printed `10..1`
   and equalled `10..1`, and two progressions differing only by step were equal.

   The driver prints each answer, reached through the vtable the way a virtual call reaches it, and
   the harness compares the lines with what `range_progression_members.kt` answers under the
   reference kotlinc. */
#include "transcript.h"

/* The three `kotlin.Any` members, through the table the way a virtual call reaches them. */
static void say_equals(KRef self, KRef other) {
    const KType *type = type_of(self);
    say_bool(((kt_boolean(*)(KRef, KRef))type->vtable[KT_SLOT_EQUALS])(self, other));
}

static void say_hash_code(KRef self) {
    const KType *type = type_of(self);
    say_long(((kt_int(*)(KRef))type->vtable[KT_SLOT_HASH_CODE])(self));
}

static void say_rendering(KRef self) {
    const KType *type = type_of(self);
    say_text(((KRef(*)(KRef))type->vtable[KT_SLOT_TO_STRING])(self));
}

void kt_program_entry(void) {
    DRIVER_BEGIN();

    KRef one_to_ten = kt_int_range(1, 10);
    KRef odd = kt_range_step(one_to_ten, 2);
    KRef ten_down = kt_int_range_down_to(10, 1);
    KRef ten_to_one = kt_int_range(10, 1);

    /* `toString`: a range is its bounds, a progression names its step, and a descending one is
       written the way it was built. */
    KRef rendered[] = {
        one_to_ten,
        odd,
        ten_down,
        kt_range_step(kt_int_range(1, 3), 1),
        kt_range_reversed(kt_range_step(kt_int_range(1, 9), 3)),
        kt_range_step(kt_long_range_down_to(5, 1), 2),
        kt_range_step(kt_char_range('a', 'e'), 2),
        kt_char_range_down_to('e', 'a'),
    };
    for (unsigned at = 0; at < sizeof(rendered) / sizeof(rendered[0]); at++) {
        say(at == 0 ? "" : " | ");
        say_rendering(rendered[at]);
    }
    say("\n");

    /* `equals`: a progression's includes the step; a range equals only a range, while a
       progression equals a range of the same walk. Two empty ones of a kind are equal. */
    say_equals(ten_down, ten_to_one);
    say(" ");
    say_equals(ten_to_one, ten_down);
    say(" ");
    say_equals(kt_range_step(kt_int_range(1, 9), 2), kt_range_step(kt_int_range(1, 9), 4));
    say(" ");
    say_equals(odd, kt_range_step(kt_int_range(1, 9), 2));
    say("\n");
    say_equals(kt_range_step(kt_int_range(1, 3), 1), kt_int_range(1, 3));
    say(" ");
    say_equals(kt_int_range(1, 3), kt_range_step(kt_int_range(1, 3), 1));
    say(" ");
    say_equals(kt_int_range_down_to(1, 10), kt_int_range(5, 1));
    say(" ");
    say_equals(kt_int_range(5, 1), kt_int_range_down_to(1, 10));
    say(" ");
    say_equals(kt_int_range(1, 3), kt_int_range(1, 3));
    say("\n");

    /* `hashCode`: `31 * (31 * first + last) + step` for a progression, each part folded through
       its own type's hash, and `31 * first + last` for a range. */
    KRef hashed[] = {
        one_to_ten,
        odd,
        ten_down,
        kt_range_step(kt_long_range_down_to(5, 1), 2),
        kt_range_step(kt_char_range('a', 'e'), 2),
        kt_int_range_down_to(1, 10),
    };
    for (unsigned at = 0; at < sizeof(hashed) / sizeof(hashed[0]); at++) {
        say(at == 0 ? "" : " ");
        say_hash_code(hashed[at]);
    }
    say("\n");

    CHECK(kt_pending_exception() == NULL, "a progression member raised\n");
    kt_sys_write(1, "OK\n", 3);
}
