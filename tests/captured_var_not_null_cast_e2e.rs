//! `x!!` over a captured `var` checks the value in the shared cell's field slot. kotlinc reads
//! `Ref$ObjectRef.element` as `Object`, null-checks that value, and only then casts it to the
//! variable's type (`getfield; dup; checkNotNull; checkcast`), the order every coroutine box test's
//! `runBlocking` helper takes for `res!!`. An erased type-parameter variable needs no cast at all.
use super::common;

const CAPTURED_VARS: &str = "package store\n\
                             \n\
                             import kotlin.coroutines.*\n\
                             \n\
                             fun <T> runBlocking(block: suspend () -> T): T {\n\
                             \x20   var res: Result<T>? = null\n\
                             \x20   block.startCoroutine(Continuation(EmptyCoroutineContext) {\n\
                             \x20       res = it\n\
                             \x20   })\n\
                             \x20   return res!!.getOrThrow()\n\
                             }\n\
                             \n\
                             fun length(f: (() -> Unit) -> Unit): Int {\n\
                             \x20   var s: String? = null\n\
                             \x20   f { s = \"a\" }\n\
                             \x20   return s!!.length\n\
                             }\n\
                             \n\
                             fun <T> generic(f: (() -> Unit) -> Unit, v: T): T {\n\
                             \x20   var s: T? = null\n\
                             \x20   f { s = v }\n\
                             \x20   return s!!\n\
                             }\n";

#[test]
fn not_null_assertion_on_a_captured_var_casts_after_the_check_like_kotlinc() {
    common::byte_diff_against_kotlinc_cp(
        "CapturedVarNotNull",
        CAPTURED_VARS,
        "store/CapturedVarNotNullKt",
        &[common::stdlib_jar()],
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("store/CapturedVarNotNullKt differs from kotlinc: {diff}"));
}
