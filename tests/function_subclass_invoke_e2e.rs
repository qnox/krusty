//! Invoking a value whose class implements a function type.
//!
//! `f(x)` on `f: F` with `class F : (Token) -> Token` is the invoke convention over `F`'s own
//! `invoke` member, not over its function supertype: kotlinc calls `F.invoke(LToken;)LToken;`
//! directly, for an explicit `f.invoke(x)` as well, and a generic subclass calls its own erased
//! `invoke(Object)Object`. Only a callable reference keeps its exact function shape beside the
//! nominal reflection type and is invoked as that function value.

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
