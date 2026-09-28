//! Calling a value whose type is a type parameter bounded by a function type.
//!
//! `t(x)` on `t: T` with `T : (Int) -> Int` is the ordinary invoke convention over the bound: the
//! value claims the call exactly as a `(Int) -> Int` value would, and kotlinc calls the erased
//! `Function1.invoke` on it.

use super::common;

/// A bound written in the type-parameter list and one written in a `where` clause are the same
/// bound; both calls box the argument and unbox the erased result through `Number`.
#[test]
fn a_function_bounded_type_parameter_value_is_invoked_like_kotlinc() {
    const SRC: &str = "fun <T : (Int) -> Int> listed(t: T): Int = t(5)\n\
fun <T> constrained(t: T): Int where T : (Int) -> Int = t(6)\n";
    common::byte_diff_against_kotlinc("FunctionBoundInvoke", SRC, "FunctionBoundInvokeKt")
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{diff}"));
}

#[test]
fn a_function_bounded_type_parameter_value_runs() {
    const SRC: &str = "fun <T : (Int) -> Int> listed(t: T): Int = t(5)\n\
fun <T> constrained(t: T): Int where T : (Int) -> Int = t(6)\n\
fun box(): String {\n\
    val double = { x: Int -> x * 2 }\n\
    return if (listed(double) == 10 && constrained(double) == 12) \"OK\" else \"fail\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "FunctionBoundInvokeRun");
}

/// A function bound that is not the first one is not the parameter's erasure: kotlinc casts the
/// value to `Function1` before invoking it. A bound reached through another type parameter is the
/// erasure already, so the chained call needs no cast, and `T` is signed as `T extends U` alone.
#[test]
fn later_and_chained_function_bounds_are_invoked_like_kotlinc() {
    const SRC: &str = "open class Base\n\
fun <T> second(t: T): Int where T : Base, T : (Int) -> Int = t(5)\n\
fun <U : (Int) -> Int, T : U> chained(t: T): Int = t(6)\n";
    common::byte_diff_against_kotlinc(
        "LaterFunctionBoundInvoke",
        SRC,
        "LaterFunctionBoundInvokeKt",
    )
    .expect("reference kotlinc is provisioned")
    .unwrap_or_else(|diff| panic!("{diff}"));
}

#[test]
fn later_and_chained_function_bounds_run() {
    const SRC: &str = "open class Base\n\
class Doubler : Base(), (Int) -> Int { override fun invoke(x: Int): Int = x * 2 }\n\
fun <T> second(t: T): Int where T : Base, T : (Int) -> Int = t(5)\n\
fun <U : (Int) -> Int, T : U> chained(t: T): Int = t(6)\n\
fun box(): String {\n\
    val doubler = Doubler()\n\
    return if (second(doubler) == 10 && chained(doubler) == 12) \"OK\" else \"fail\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "LaterFunctionBoundInvokeRun");
}

/// A value that may be null cannot be invoked directly, whether its type is nullable or it is a
/// type parameter whose every bound admits null, followed through a bound that is another type
/// parameter. kotlinc names a type parameter with its owner and that owner's written bounds. One
/// non-null bound makes the value non-null even when its function bound is nullable. A member `var`
/// is not stable, so assigning it a lambda does not make it callable; a checked or initialized `val`
/// is.
#[test]
fn a_nullable_function_value_or_bound_is_not_invoked_directly() {
    const SRC: &str = "interface Base\n\
fun value(f: (() -> Unit)?) { f() }\n\
fun <A, T : ((Int) -> A)?> listed(t: T, a: Int) { t(a) }\n\
fun <T> constrained(t: T) where T : Base?, T : (() -> Unit)? { t() }\n\
fun <U : (() -> Unit)?, T : U> chained(t: T) { t() }\n\
class Holder<out T : (() -> Unit)?>(val t: T) { fun run() { t() } }\n\
class Outer { fun <T : (() -> Unit)?> member(t: T) { t() } }\n\
fun <T> nonNull(t: T) where T : Base, T : (() -> Unit)? { t() }\n\
class Mutable { var g: (() -> Unit)? = null; fun f() { g = {}; g() } }\n\
class Checked(val g: (() -> Unit)?) { fun f() { if (g != null) g() } }\n\
class Initialized { val g: (() -> Unit)?; init { g = {}; g() } }\n";
    common::assert_errors_match_kotlinc(&[("Main.kt", SRC)], &[]);
}

/// A smart cast makes a nullable function-typed member callable: a null check on a `val`, or its
/// initializing assignment in `init`, including one in a branch.
#[test]
fn a_smart_cast_member_function_value_runs() {
    const SRC: &str = "class Checked(arg: (() -> Unit)?) {\n\
    val func: (() -> Unit)? = arg\n\
    init { if (func != null) func() }\n\
}\n\
class Initialized {\n\
    val func: (() -> Unit)?\n\
    init { if (true) { func = {}; func() } else { func = null } }\n\
}\n\
fun box(): String {\n\
    Checked({})\n\
    Initialized()\n\
    return \"OK\"\n\
}\n";
    common::expect_box_ok_with_stdlib(SRC, "SmartCastMemberFunctionInvoke");
}
