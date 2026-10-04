//! Invoking a value whose class implements a function type.
//!
//! `f(x)` on `f: F` with `class F : (Token) -> Token` is the invoke convention over `F`'s own
//! `invoke` member, not over its function supertype: kotlinc calls `F.invoke(LToken;)LToken;`
//! directly, for an explicit `f.invoke(x)` as well, and a generic subclass calls its own erased
//! `invoke(Object)Object`. Only a callable reference keeps its exact function shape beside the
//! nominal reflection type and is invoked as that function value.
//!
//! The member is reached through the function classifier the supertype instantiates, for a suspend
//! or big-arity function type as well (`SuspendFunction1`, `Function23`), and for a function-type
//! supertype in a dependency's metadata: a property reference value calls `KProperty0.invoke`.

use super::common;

const SRC: &str = "class Token(val n: Int)\n\
class Next : (Token) -> Token { override fun invoke(t: Token): Token = Token(t.n + 1) }\n\
open class Same<T> : (T) -> T { override fun invoke(x: T): T = x }\n\
class Both : (Token, Token) -> Token { override fun invoke(a: Token, b: Token): Token = Token(a.n + b.n) }\n\
fun implicit(f: Next, t: Token): Token = f(t)\n\
fun explicit(f: Next, t: Token): Token = f.invoke(t)\n\
fun generic(g: Same<Token>, t: Token): Token = g(t)\n\
fun two(b: Both, t: Token): Token = b(t, t)\n";

#[test]
fn a_function_subclass_value_calls_its_own_invoke_like_kotlinc() {
    common::byte_diff_against_kotlinc("FunctionSubclassInvoke", SRC, "FunctionSubclassInvokeKt")
        .expect("reference kotlinc is provisioned")
        .unwrap_or_else(|diff| panic!("{diff}"));
}

#[test]
fn a_function_subclass_value_runs_its_own_invoke() {
    let source = format!(
        "{SRC}fun box(): String {{\n\
    val t = Token(1)\n\
    val sum = implicit(Next(), t).n + explicit(Next(), t).n + generic(Same(), t).n + two(Both(), t).n\n\
    return if (sum == 7) \"OK\" else \"fail $sum\"\n\
}}\n"
    );
    common::expect_box_ok_with_stdlib(&source, "FunctionSubclassInvokeRun");
}

const SHAPES_SRC: &str = "class Token(val n: Int)\n\
class Later : suspend (Token) -> Token { override suspend fun invoke(t: Token): Token = Token(t.n + 1) }\n\
suspend fun later(f: Later, t: Token): Token = f(t)\n\
class Wide : (Token, Token, Token, Token, Token, Token, Token, Token, Token, Token, Token, Token, Token, Token, Token, Token, Token, Token, Token, Token, Token, Token, Token) -> Int {\n\
    override fun invoke(a: Token, b: Token, c: Token, d: Token, e: Token, f: Token, g: Token, h: Token, i: Token, j: Token, k: Token, l: Token, m: Token, n: Token, o: Token, p: Token, q: Token, r: Token, s: Token, t: Token, u: Token, v: Token, w: Token): Int = a.n + w.n\n\
}\n\
fun wide(f: Wide, t: Token): Int = f(t, t, t, t, t, t, t, t, t, t, t, t, t, t, t, t, t, t, t, t, t, t, t)\n\
class Holder(val count: Int) {\n\
    fun viaProperty(): Int = pass(::count) { it() }\n\
}\n\
fun <T, R> pass(value: T, f: (T) -> R): R = f(value)\n";

#[test]
fn suspend_big_arity_and_reference_values_call_invoke_like_kotlinc() {
    let stdlib = [common::stdlib_jar()];
    for class in ["FunctionShapeInvokeKt", "Wide", "Holder"] {
        common::byte_diff_against_kotlinc_cp("FunctionShapeInvoke", SHAPES_SRC, class, &stdlib)
            .expect("reference kotlinc is provisioned")
            .unwrap_or_else(|diff| panic!("{class}: {diff}"));
    }
}

#[test]
fn big_arity_and_reference_values_run_their_invoke() {
    let source = format!(
        "{SHAPES_SRC}fun box(): String {{\n\
    val holder = Holder(3)\n\
    val sum = wide(Wide(), Token(1)) + holder.viaProperty()\n\
    return if (sum == 5) \"OK\" else \"fail $sum\"\n\
}}\n"
    );
    common::expect_box_ok_with_stdlib(&source, "FunctionShapeInvokeRun");
}
