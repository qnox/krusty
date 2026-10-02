/* Unboxing a null `UByte?`/`UShort?`/`UInt?`/`ULong?` is Kotlin's `NullPointerException`, which a
   program may catch. `kt_throw` RECORDS the exception and returns, so each unboxer must return too:
   it used to fall through and read the value out of the null it had just rejected, which crashed
   the program where Kotlin hands the `catch` its exception.

   The driver prints what each unboxing raised, and the harness compares the lines with what
   `unsigned_unbox_null.kt` answers under the reference kotlinc. */
#include "transcript.h"

/* The unboxer answered, and what it recorded is the `NullPointerException` a `!!` raises: the
   transcript's line for it, which also clears the slot, as the `catch` that takes it would. */
static void expect_null_pointer_exception(const char *label) {
    KRef thrown = kt_pending_exception();
    if (thrown == NULL) {
        KT_SYS_FAIL("unboxing null recorded no exception\n");
    }
    if (((const KObjectHeader *)thrown)->type != &kt_type_null_pointer_exception) {
        KT_SYS_FAIL("unboxing null recorded something other than a NullPointerException\n");
    }
    say_raised(label);
}

__attribute__((noinline)) static void run(void) {
    kt_unbox_ubyte(NULL);
    expect_null_pointer_exception("UByte?!!");
    kt_unbox_ushort(NULL);
    expect_null_pointer_exception("UShort?!!");
    kt_unbox_uint(NULL);
    expect_null_pointer_exception("UInt?!!");
    kt_unbox_ulong(NULL);
    expect_null_pointer_exception("ULong?!!");

    /* A box that is there still answers its value, read back as the bits it was given. */
    if (kt_unbox_uint(kt_box_uint(-1)) != -1 || kt_unbox_ulong(kt_box_ulong(-2)) != -2 ||
        kt_unbox_ubyte(kt_box_ubyte(-3)) != -3 || kt_unbox_ushort(kt_box_ushort(-4)) != -4) {
        KT_SYS_FAIL("an unsigned box did not answer its value\n");
    }
    if (kt_pending_exception() != NULL) {
        KT_SYS_FAIL("unboxing a value recorded an exception\n");
    }
}

void kt_program_entry(void) {
    void *stack_bottom = NULL;
    kt_runtime_init(&stack_bottom);
    run();
    kt_sys_write(1, "OK\n", 3);
}
