/* An exhaustive `when` that no branch matched raises `kotlin.NoWhenBranchMatchedException`, a
   `RuntimeException` with no message, which a program may catch. It used to stop the program with
   `krusty: no branch of an exhaustive `when` matched`. Kotlin declares the class itself, so both
   platforms name it `kotlin.NoWhenBranchMatchedException`.

   The driver prints the exception's name, message, kind and rendering, and the harness compares
   the line with what `no_when_branch_matched.kt` answers under the reference kotlinc. */
#include "driver_exceptions.h"
#include "transcript.h"

void kt_program_entry(void) {
    DRIVER_BEGIN();
    kt_no_when_branch_matched();
    KRef thrown = kt_pending_exception();
    CHECK(thrown != NULL, "an unmatched when raised nothing\n");
    kt_clear_pending();
    CHECK(kt_is_instance(thrown, &kt_type_runtime_exception),
          "NoWhenBranchMatchedException is not a RuntimeException\n");
    static const char name[] = "kotlin.NoWhenBranchMatchedException";
    const KType *type = type_of(thrown);
    CHECK(type->qualified_name_length == sizeof(name) - 1 &&
              text_is(kt_string_utf8(type->qualified_name, (kt_int)type->qualified_name_length),
                      name, sizeof(name) - 1),
          "the exception is not kotlin.NoWhenBranchMatchedException\n");
    CHECK(text_is(kt_to_string(thrown), name, sizeof(name) - 1),
          "the exception does not render as its name alone\n");
    /* `"${e::class.qualifiedName} ${e.message} ${e is RuntimeException} $e"`. */
    say_bytes_of(type->qualified_name, type->qualified_name_length);
    say(" ");
    say_value(kt_throwable_message(thrown));
    say(" ");
    say_value(kt_box_boolean(kt_is_instance(thrown, &kt_type_runtime_exception)));
    say(" ");
    say_value(thrown);
    say("\n");
    CHECK(type == &kt_type_no_when_branch_matched_exception && kt_throwable_message(thrown) == NULL,
          "the exception carries a message\n");
    kt_sys_write(1, "OK\n", 3);
}
