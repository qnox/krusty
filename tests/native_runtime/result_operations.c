/* `kotlin.Result`'s operations, on a success holding a value, a success holding `null`, and a
   failure. A success is its value and a failure a marker around the exception, so each operation
   is checked by identity as well as by answer. `getOrThrow` on a failure throws the exception it
   holds and answers NULL at once; it used to throw and then fall through to hand back the failure
   marker itself.

   The expected answers are Kotlin's, from this program compiled and run with the reference kotlinc
   2.4.10 on the JVM:

       class Exception2(val text: String) : Exception() { override fun toString() = text }
       fun main() {
           val ok: Result<Any?> = Result.success(1)
           val nul: Result<Any?> = Result.success(null)
           val e = IllegalStateException("boom")
           val bad: Result<Any?> = Result.failure(e)
           println("ok ${ok.isSuccess} ${ok.isFailure} ${ok.getOrNull()} ${ok.exceptionOrNull()} ${ok.getOrThrow()} $ok")
           println("nul ${nul.isSuccess} ${nul.isFailure} ${nul.getOrNull()} ${nul.exceptionOrNull()} ${nul.getOrThrow()} $nul")
           println("bad ${bad.isSuccess} ${bad.isFailure} ${bad.getOrNull()} ${bad.exceptionOrNull() === e}")
           try { bad.getOrThrow() } catch (t: Throwable) { println("getOrThrow threw same=${t === e}") }
           println(Result.failure<Int>(Exception2("E: boom")))
       }

   which prints `ok true false 1 null 1 Success(1)`, `nul true false null null null Success(null)`,
   `bad false true null true`, `getOrThrow threw same=true` and `Failure(E: boom)`. */
#include "driver_checks.h"

/* An exception whose `toString` is its own, as `Exception2` above: the runtime renders a failure
   through it. */
static KRef exception_to_string(KRef self) {
    (void)self;
    return kt_string_utf8("E: boom", 7);
}

static const kt_fn exception_vtable[] = {(kt_fn)kt_any_equals, (kt_fn)kt_any_hash_code,
                                         (kt_fn)exception_to_string};

static const KType exception_type = {
    .name = "pkg.Exception2",
    .name_length = sizeof("pkg.Exception2") - 1,
    .instance_size = sizeof(DriverThrowable),
    .reference_count = sizeof(driver_throwable_offsets) / sizeof(driver_throwable_offsets[0]),
    .reference_offsets = driver_throwable_offsets,
    .super = &kt_type_any,
    .vtable = exception_vtable,
    .vtable_length = 3,
};

void kt_program_entry(void) {
    DRIVER_BEGIN();

    /* Result.success(1) */
    KRef one = kt_box_int(1);
    KRef ok = kt_result_success(one);
    CHECK(kt_result_is_success(ok) && !kt_result_is_failure(ok), "success(1) is not a success\n");
    CHECK(kt_result_get_or_null(ok) == one, "success(1).getOrNull()\n");
    CHECK(kt_result_exception_or_null(ok) == NULL, "success(1).exceptionOrNull()\n");
    CHECK(kt_result_get_or_throw(ok) == one, "success(1).getOrThrow()\n");
    CHECK(kt_pending_exception() == NULL, "success(1).getOrThrow() threw\n");
    KRef rendered = kt_result_to_string(ok);
    CHECK(kt_pending_exception() == NULL && text_is(rendered, "Success(1)", 10),
          "success(1).toString()\n");

    /* Result.success(null): a success, distinct from any failure. */
    KRef nul = kt_result_success(NULL);
    CHECK(kt_result_is_success(nul) && !kt_result_is_failure(nul),
          "success(null) is not a success\n");
    CHECK(kt_result_get_or_null(nul) == NULL, "success(null).getOrNull()\n");
    CHECK(kt_result_exception_or_null(nul) == NULL, "success(null).exceptionOrNull()\n");
    CHECK(kt_result_get_or_throw(nul) == NULL, "success(null).getOrThrow()\n");
    CHECK(kt_pending_exception() == NULL, "success(null).getOrThrow() threw\n");
    rendered = kt_result_to_string(nul);
    CHECK(kt_pending_exception() == NULL && text_is(rendered, "Success(null)", 13),
          "success(null).toString()\n");

    /* Result.failure(e) */
    KRef e = kt_throwable_new(&exception_type, NULL);
    KRef bad = kt_result_failure(e);
    CHECK(!kt_result_is_success(bad) && kt_result_is_failure(bad), "failure(e) is not a failure\n");
    CHECK(kt_result_get_or_null(bad) == NULL, "failure(e).getOrNull()\n");
    CHECK(kt_result_exception_or_null(bad) == e, "failure(e).exceptionOrNull() is not e\n");
    rendered = kt_result_to_string(bad);
    CHECK(kt_pending_exception() == NULL && text_is(rendered, "Failure(E: boom)", 16),
          "failure(e).toString()\n");

    /* getOrThrow on the failure: e is thrown, by identity, and the answer is NULL rather than the
       marker. */
    KRef answer = kt_result_get_or_throw(bad);
    CHECK(kt_pending_exception() == e, "failure(e).getOrThrow() did not throw e\n");
    CHECK(answer == NULL, "failure(e).getOrThrow() answered the failure marker\n");
    kt_clear_pending();

    kt_sys_write(1, "OK\n", 3);
}
